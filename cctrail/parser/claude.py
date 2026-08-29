from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import orjson

from ..model import Event, Provider, Session, Speed, Usage
from . import AgentCall, inherit_roots, jsonl, jsonl_files


@dataclass
class AgentFallback:
    id: str
    timestamp: datetime | None
    usage: Usage
    speed: Speed | None


@dataclass
class Parsed:
    path: Path
    session: Session
    calls: list[AgentCall] = field(default_factory=list)
    fallbacks: list[AgentFallback] = field(default_factory=list)


def parse(root: Path) -> list[Session]:
    projects = root / "projects"
    root = projects if projects.is_dir() else root
    if not root.is_dir():
        return []

    parsed = [parse_file(path) for path in jsonl_files([root])]
    agents: dict[tuple[Path, str], int] = {}
    for i, item in enumerate(parsed):
        if item.session.is_main or (directory := session_dir(item.path)) is None:
            continue
        agents[directory, item.session.id] = i
        if id := tool_use_id(item.path):
            agents[directory, id] = i

    parents: dict[int, int] = {}
    for i, item in enumerate(parsed):
        directory = main_session_dir(item.path) if item.session.is_main else session_dir(item.path)
        if directory:
            link_agents(i, directory, agents, parsed, parents)

    seen_fallbacks: set[tuple[int, str]] = set()
    for parent in range(len(parsed)):
        for fallback in parsed[parent].fallbacks:
            has_log = any(
                call.resolved and call.agent_id == fallback.id for call in parsed[parent].calls
            )
            key = parent, fallback.id
            if has_log or key in seen_fallbacks:
                continue
            seen_fallbacks.add(key)
            child = len(parsed)
            parents[child] = parent
            parsed.append(
                Parsed(
                    path=Path(),
                    session=Session(
                        id=fallback.id,
                        root_id=fallback.id,
                        is_main=False,
                        workdir="unknown",
                        provider=Provider.CLAUDE,
                        events=[
                            Event(
                                timestamp=fallback.timestamp,
                                usage=fallback.usage,
                                speed=fallback.speed,
                            )
                        ],
                    ),
                )
            )

    sessions = [x.session for x in parsed]
    inherit_roots(sessions, parents)
    return [x for x in sessions if x.events]


def parse_file(path: Path) -> Parsed:
    id = path.stem.removeprefix("agent-") or "unknown"
    session = Session(
        id=id,
        root_id=id,
        is_main=not is_agent(path),
        workdir="unknown",
        provider=Provider.CLAUDE,
    )
    calls: list[AgentCall] = []
    fallbacks: list[AgentFallback] = []
    turns: dict[tuple[str, str, str | None], int] = {}

    for line in jsonl(path):
        if session.workdir == "unknown" and isinstance(line.get("cwd"), str):
            session.workdir = line["cwd"]
        kind = line.get("type")
        if kind == "assistant":
            record_assistant(session, calls, turns, line)
        elif kind == "user" and not line.get("isMeta", False):
            record_user(session, calls, fallbacks, line)
    return Parsed(path, session, calls, fallbacks)


def record_assistant(
    session: Session,
    calls: list[AgentCall],
    turns: dict[tuple[str, str, str | None], int],
    line: dict[str, Any],
) -> None:
    message = line.get("message")
    if not isinstance(message, dict):
        return
    content = message.get("content")
    record_spawns(calls, content)
    timestamp = parse_timestamp(line.get("timestamp"))
    id = message.get("id") if isinstance(message.get("id"), str) else None
    request = line.get("requestId") if isinstance(line.get("requestId"), str) else None
    identity = (id, request) if id is not None else None
    anonymous = request or line.get("uuid")
    key: tuple[str, str, str | None] | None
    key = ("message", id, request) if id is not None else None
    if key is None and isinstance(anonymous, str):
        key = ("anonymous", anonymous, None)

    index = turns.get(key) if key is not None else None
    if index is None:
        session.events.append(
            Event(
                timestamp=timestamp,
                identity=identity,
                sidechain=bool(line.get("isSidechain", False)),
                failed=bool(line.get("isApiErrorMessage", False)),
            )
        )
        index = len(session.events) - 1
        if key is not None:
            turns[key] = index

    event = session.events[index]
    event.failed |= bool(line.get("isApiErrorMessage", False))
    event.sidechain |= bool(line.get("isSidechain", False))
    if event.model is None and isinstance(message.get("model"), str):
        event.model = message["model"]
    raw = message.get("usage")
    if isinstance(raw, dict):
        usage = normalize_usage(raw)
        if event.usage is None or usage.total > event.usage.total:
            event.usage = usage
        event.speed = read_speed(raw) or event.speed


def record_user(
    session: Session,
    calls: list[AgentCall],
    fallbacks: list[AgentFallback],
    line: dict[str, Any],
) -> None:
    message = line.get("message")
    if not isinstance(message, dict):
        return
    content = message.get("content")
    if has_tool_result(content):
        result = line.get("toolUseResult")
        if isinstance(result, dict) and isinstance(result.get("agentId"), str):
            id = result["agentId"]
            if (tool_id := tool_result_id(content)) and (
                call := next((x for x in calls if x.tool_id == tool_id), None)
            ):
                call.agent_id = id
            raw = result.get("usage")
            raw = raw if isinstance(raw, dict) else {}
            fallbacks.append(
                AgentFallback(
                    id=id,
                    timestamp=parse_timestamp(line.get("timestamp")),
                    usage=normalize_usage(raw),
                    speed=read_speed(raw),
                )
            )
        return
    if is_user_text(content) and (timestamp := parse_timestamp(line.get("timestamp"))):
        session.events.append(Event.activity(timestamp))


def normalize_usage(raw: dict[str, Any]) -> Usage:
    cache = raw.get("cache_creation")
    cache = cache if isinstance(cache, dict) else {}
    cache_5m = integer(cache.get("ephemeral_5m_input_tokens"))
    cache_1h = integer(cache.get("ephemeral_1h_input_tokens"))
    result = Usage(
        input=integer(raw.get("input_tokens")),
        cache_write_5m=(
            integer(raw.get("cache_creation_input_tokens"))
            if cache_5m + cache_1h == 0
            else cache_5m
        ),
        cache_write_1h=cache_1h,
        cache_read=integer(raw.get("cache_read_input_tokens")),
        output=integer(raw.get("output_tokens")),
    )
    iterations = raw.get("iterations")
    if isinstance(iterations, list):
        for iteration in iterations:
            if isinstance(iteration, dict) and iteration.get("type") == "advisor_message":
                result += normalize_usage(iteration)
    return result


def read_speed(raw: dict[str, Any]) -> Speed | None:
    value = raw.get("speed") or raw.get("service_tier")
    return Speed.parse(value) if isinstance(value, str) else None


def is_user_text(content: Any) -> bool:
    if isinstance(content, str):
        return bool(content.strip())
    if not isinstance(content, list):
        return False
    return any(
        isinstance(block, dict)
        and isinstance(block.get("text") or block.get("input_text"), str)
        and bool((block.get("text") or block.get("input_text")).strip())
        for block in content
    )


def has_tool_result(content: Any) -> bool:
    return isinstance(content, list) and any(
        isinstance(x, dict) and x.get("type") == "tool_result" for x in content
    )


def record_spawns(calls: list[AgentCall], content: Any) -> None:
    if not isinstance(content, list):
        return
    for block in content:
        if not isinstance(block, dict) or block.get("type") != "tool_use":
            continue
        id = block.get("id")
        if (
            not isinstance(id, str)
            or not id
            or block.get("name") not in {"Agent", "Task"}
            or any(x.tool_id == id for x in calls)
        ):
            continue
        calls.append(AgentCall(id))


def tool_result_id(content: Any) -> str | None:
    if not isinstance(content, list):
        return None
    for block in content:
        if isinstance(block, dict) and block.get("type") == "tool_result":
            id = block.get("tool_use_id")
            return id if isinstance(id, str) else None
    return None


def is_agent(path: Path) -> bool:
    return "subagents" in path.parts


def main_session_dir(path: Path) -> Path:
    return path.parent / path.stem


def session_dir(path: Path) -> Path | None:
    for directory in path.parents:
        if directory.name == "subagents":
            return directory.parent
    return None


def tool_use_id(path: Path) -> str | None:
    try:
        value = orjson.loads(path.with_suffix(".meta.json").read_bytes())
    except (OSError, orjson.JSONDecodeError):
        return None
    id = value.get("toolUseId") if isinstance(value, dict) else None
    return id if isinstance(id, str) else None


def link_agents(
    parent: int,
    directory: Path,
    agents: dict[tuple[Path, str], int],
    parsed: list[Parsed],
    parents: dict[int, int],
) -> None:
    calls = [
        (i, call.agent_id or call.tool_id)
        for i, call in enumerate(parsed[parent].calls)
        if not call.resolved
    ]
    for call, key in calls:
        child = agents.get((directory, key))
        if child is None:
            continue
        parsed[parent].calls[call].resolved = True
        if child not in parents:
            parents[child] = parent
            link_agents(child, directory, agents, parsed, parents)


def parse_timestamp(value: Any) -> datetime | None:
    if not isinstance(value, str):
        return None
    try:
        timestamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return timestamp.replace(tzinfo=UTC) if timestamp.tzinfo is None else timestamp


def integer(value: Any) -> int:
    return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else 0
