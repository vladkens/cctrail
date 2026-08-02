from collections.abc import Iterator
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any

from .models import Message, Session, ToolCall, Usage
from .utils import (
    extract_text,
    imap,
    int_or_zero,
    json_dump,
    json_lines,
    json_load,
    parse_timestamp,
    string_or_none,
)

PARALLEL_MIN_FILES = 32


@dataclass(frozen=True)
class Rollout:
    path: Path
    id: str
    parent_id: str | None
    forked_from_id: str | None
    has_history_base: bool
    own_start_ordinal: int | None
    agent_type: str | None
    agent_name: str | None
    cwd: str | None
    git_branch: str | None


def default_root() -> Path:
    return Path.home() / ".codex"


def iter_sessions(root: str | Path | None = None) -> Iterator[Session]:
    """Stream Codex sessions, attaching spawned agents to their tool calls."""
    root = Path(root) if root is not None else default_root()
    titles = load_titles(root)
    rollouts = [rollout for path in history_files(root) if (rollout := read_meta(path))]
    by_id = {rollout.id: rollout for rollout in rollouts}
    rollouts = list(by_id.values())
    paths = replay_paths(rollouts, by_id)

    workers = None if len(rollouts) >= PARALLEL_MIN_FILES else 1
    replay_events = dict(zip(paths, imap(usage_events, paths, workers=workers)))
    jobs = parse_jobs(rollouts, by_id, titles, replay_events)
    parsed = list(imap(parse_job, jobs, workers=workers))

    sessions = {session.id: session for session in parsed}

    attached: set[str] = set()
    for rollout in by_id.values():
        parent = sessions.get(rollout.parent_id or "")
        if parent is not None and attach_agent(parent, sessions[rollout.id]):
            attached.add(rollout.id)

    for session_id, session in sessions.items():
        if session_id not in attached and session.messages:
            yield session


def parse_jobs(
    rollouts: list[Rollout],
    by_id: dict[str, Rollout],
    titles: dict[str, str],
    replay_events: dict[Path, list[tuple[int, tuple[int, ...]]]],
) -> list[tuple[Rollout, str | None, int]]:
    return [
        (rollout, titles.get(rollout.id), replay_end(rollout, by_id, replay_events))
        for rollout in rollouts
    ]


def parse_job(job: tuple[Rollout, str | None, int]) -> Session:
    return parse_file(*job)


def history_files(root: Path) -> list[Path]:
    directories = [root / "sessions", root / "archived_sessions"]
    paths = [
        path
        for directory in directories
        if directory.is_dir()
        for path in directory.rglob("*.jsonl")
    ]
    if not paths and root.is_dir():
        paths = list(root.rglob("*.jsonl"))

    return sorted(paths)


def read_meta(path: Path) -> Rollout | None:
    item = next(json_lines(path), None)
    if not isinstance(item, dict) or item.get("type") != "session_meta":
        return None

    payload = item.get("payload")
    if not isinstance(payload, dict):
        return None

    thread_id = string_or_none(payload.get("id"))
    if thread_id is None:
        return None

    git = payload.get("git")
    return Rollout(
        path=path,
        id=thread_id,
        parent_id=parent_id(payload),
        forked_from_id=string_or_none(payload.get("forked_from_id")),
        has_history_base=isinstance(payload.get("history_base"), dict),
        own_start_ordinal=integer(payload.get("subagent_history_start_ordinal")),
        agent_type=string_or_none(payload.get("agent_role"))
        or string_or_none(payload.get("agent_type")),
        agent_name=string_or_none(payload.get("agent_nickname")),
        cwd=string_or_none(payload.get("cwd")),
        git_branch=string_or_none(git.get("branch")) if isinstance(git, dict) else None,
    )


def parse_file(rollout: Rollout, title: str | None, replay_end: int) -> Session:
    session = Session(
        id=rollout.id,
        kind="agent" if rollout.parent_id else "main",
        provider="codex",
        agent_type=rollout.agent_type,
        title=title or rollout.agent_name or rollout.agent_type,
        cwd=rollout.cwd,
        git_branch=rollout.git_branch,
    )
    pending_tools: dict[str, ToolCall] = {}
    current_assistant: Message | None = None
    current_user: Message | None = None
    current_model: str | None = None
    turn_started_at: datetime | None = None
    last_cumulative: tuple[int, ...] | None = None

    def assistant(timestamp: datetime | None) -> Message:
        nonlocal current_assistant
        if current_assistant is None:
            current_assistant = Message(role="assistant", timestamp=timestamp, model=current_model)
            session.messages.append(current_assistant)
        elif current_assistant.model is None:
            current_assistant.model = current_model

        return current_assistant

    for line_number, item in enumerate(json_lines(rollout.path)):
        payload = item.get("payload")
        if not isinstance(payload, dict):
            continue

        timestamp = parse_timestamp(item.get("timestamp"))
        item_type = item.get("type")

        if item_type == "session_meta":
            continue

        ordinal = integer(item.get("ordinal"))
        inherited = line_number <= replay_end or (
            rollout.own_start_ordinal is not None
            and ordinal is not None
            and ordinal < rollout.own_start_ordinal
        )
        if inherited:
            if item_type == "event_msg" and payload.get("type") == "token_count":
                info = payload.get("info")
                if isinstance(info, dict):
                    cumulative = usage_signature(info.get("total_token_usage"))
                    if cumulative is not None:
                        last_cumulative = cumulative

            continue

        if item_type == "turn_context":
            current_model = string_or_none(payload.get("model")) or current_model
            session.cwd = string_or_none(payload.get("cwd")) or session.cwd
            continue

        if item_type == "event_msg":
            subtype = payload.get("type")
            if subtype == "task_started":
                current_user = None
                turn_started_at = parse_timestamp(payload.get("started_at")) or timestamp
            elif subtype == "user_message":
                text = string_or_none(payload.get("message"))
                if text:
                    current_user = Message(role="user", timestamp=timestamp, text=text)
                    session.messages.append(current_user)

                current_assistant = None
            elif subtype == "task_complete":
                if current_user and turn_started_at and timestamp:
                    current_user.turn_duration = timestamp - turn_started_at

                current_assistant = None
                turn_started_at = None
            elif subtype == "token_count":
                info = payload.get("info")
                if not isinstance(info, dict):
                    continue

                cumulative = usage_signature(info.get("total_token_usage"))
                if cumulative is not None and cumulative == last_cumulative:
                    continue

                last_cumulative = cumulative
                message_usage = usage_from_raw(info.get("last_token_usage"))
                if message_usage is not None:
                    message = assistant(timestamp)
                    message.usage = message_usage
                    message.timestamp = timestamp or message.timestamp

                current_assistant = None
            continue

        if item_type != "response_item":
            continue

        subtype = payload.get("type")
        if subtype == "message" and payload.get("role") == "assistant":
            text = extract_text(payload.get("content"), ("output_text", "text"))
            if text:
                message = assistant(timestamp)
                message.text = "\n".join(filter(None, (message.text, text)))
        elif subtype in {"function_call", "custom_tool_call", "tool_search_call"}:
            call_id = (
                string_or_none(payload.get("call_id")) or string_or_none(payload.get("id")) or ""
            )
            call = ToolCall(
                id=call_id,
                name=string_or_none(payload.get("name")) or subtype.removesuffix("_call"),
                input=tool_input(payload),
                started_at=timestamp,
            )
            assistant(timestamp).tool_calls.append(call)
            pending_tools[call_id] = call
        elif subtype in {"function_call_output", "custom_tool_call_output", "tool_search_output"}:
            call_id = (
                string_or_none(payload.get("call_id")) or string_or_none(payload.get("id")) or ""
            )
            call = pending_tools.get(call_id)
            if call is not None:
                output = payload.get("output")
                call.ended_at = timestamp
                call.output_text = output_text(output)
                call.raw_result = json_value(output)
                call.is_error = is_error(call.raw_result)
        elif subtype in {"web_search_call", "image_generation_call"}:
            status = payload.get("status")
            assistant(timestamp).tool_calls.append(
                ToolCall(
                    id=string_or_none(payload.get("call_id"))
                    or string_or_none(payload.get("id"))
                    or "",
                    name=subtype.removesuffix("_call"),
                    input=tool_input(payload),
                    started_at=timestamp,
                    ended_at=timestamp if status == "completed" else None,
                    raw_result=payload,
                    is_error=status == "failed",
                )
            )

    return session


def parent_id(payload: dict[str, Any]) -> str | None:
    direct = string_or_none(payload.get("parent_thread_id"))
    return (
        direct
        or nested_parent_id(payload.get("source"))
        or nested_parent_id(payload.get("thread_source"))
    )


def nested_parent_id(value: Any) -> str | None:
    if isinstance(value, dict):
        parent_id = string_or_none(value.get("parent_thread_id"))
        if parent_id:
            return parent_id
        for child in value.values():
            if child_parent_id := nested_parent_id(child):
                return child_parent_id

    elif isinstance(value, list):
        for child in value:
            if child_parent_id := nested_parent_id(child):
                return child_parent_id
    return None


def replay_end(
    rollout: Rollout,
    by_id: dict[str, Rollout],
    replay_events: dict[Path, list[tuple[int, tuple[int, ...]]]],
) -> int:
    """Return the last legacy line copied from a fork's source rollout."""
    parent = replay_parent(rollout, by_id)
    if parent is None:
        return -1

    parent_usage = [usage for _, usage in replay_events[parent.path]]
    if not parent_usage:
        return -1

    parent_position = 0
    replay_end = -1
    for line_number, usage in replay_events[rollout.path]:
        while parent_position < len(parent_usage) and parent_usage[parent_position] != usage:
            parent_position += 1

        if parent_position == len(parent_usage):
            break

        parent_position += 1
        replay_end = line_number

    return replay_end


def replay_paths(rollouts: list[Rollout], by_id: dict[str, Rollout]) -> list[Path]:
    paths: set[Path] = set()
    for rollout in rollouts:
        parent = replay_parent(rollout, by_id)
        if parent is not None:
            paths.update((rollout.path, parent.path))

    return sorted(paths)


def replay_parent(rollout: Rollout, by_id: dict[str, Rollout]) -> Rollout | None:
    if (
        rollout.forked_from_id is None
        or rollout.has_history_base
        or rollout.own_start_ordinal is not None
    ):
        return None

    return by_id.get(rollout.forked_from_id)


def usage_events(path: Path) -> list[tuple[int, tuple[int, ...]]]:
    events: list[tuple[int, tuple[int, ...]]] = []
    previous_total: tuple[int, ...] | None = None
    for line_number, item in enumerate(json_lines(path)):
        payload = item.get("payload")
        if (
            item.get("type") != "event_msg"
            or not isinstance(payload, dict)
            or payload.get("type") != "token_count"
        ):
            continue

        info = payload.get("info")
        if not isinstance(info, dict):
            continue

        total = usage_signature(info.get("total_token_usage"))
        if total is not None and total == previous_total:
            continue

        previous_total = total
        usage = usage_signature(info.get("last_token_usage"))
        if usage is not None:
            events.append((line_number, usage))

    return events


def load_titles(root: Path) -> dict[str, str]:
    candidates = set(root.rglob("session_index.jsonl")) if root.is_dir() else set()
    candidates.add(root.parent / "session_index.jsonl")

    titles: dict[str, str] = {}
    for path in sorted(candidates):
        if not path.is_file():
            continue
        for item in json_lines(path):
            session_id = string_or_none(item.get("id"))
            title = string_or_none(item.get("thread_name"))
            if session_id and title:
                titles[session_id] = title.strip()
    return titles


def attach_agent(parent: Session, agent: Session) -> bool:
    spawn_calls = [call for call in parent.iter_tool_calls() if call.name.endswith("spawn_agent")]
    call = next((call for call in spawn_calls if agent_id(call.raw_result) == agent.id), None)
    call = call or next((call for call in spawn_calls if call.agent is None), None)
    if call is None:
        return False

    call.agent = agent
    agent.agent_type = agent.agent_type or string_or_none(call.input.get("agent_type"))
    agent.title = agent.title or string_or_none(call.input.get("message")) or agent.agent_type
    return True


def agent_id(value: Any) -> str | None:
    if isinstance(value, dict):
        value_agent_id = string_or_none(value.get("agent_id"))
        if value_agent_id:
            return value_agent_id

        for child in value.values():
            if child_agent_id := agent_id(child):
                return child_agent_id
    elif isinstance(value, list):
        for child in value:
            if child_agent_id := agent_id(child):
                return child_agent_id
    return None


def usage_from_raw(value: Any) -> Usage | None:
    if not isinstance(value, dict):
        return None

    input_tokens = int_or_zero(value.get("input_tokens"))
    cache_read = min(int_or_zero(value.get("cached_input_tokens")), input_tokens)
    output_tokens = int_or_zero(value.get("output_tokens"))
    reasoning = min(int_or_zero(value.get("reasoning_output_tokens")), output_tokens)
    return Usage(
        input=input_tokens - cache_read,
        cache_write=int_or_zero(value.get("cache_write_input_tokens")),
        cache_read=cache_read,
        output=output_tokens - reasoning,
        reasoning=reasoning,
    )


def usage_signature(value: Any) -> tuple[int, ...] | None:
    if not isinstance(value, dict):
        return None

    return tuple(
        int_or_zero(value.get(key))
        for key in (
            "input_tokens",
            "cached_input_tokens",
            "cache_write_input_tokens",
            "output_tokens",
            "reasoning_output_tokens",
            "total_tokens",
        )
    )


def tool_input(payload: dict[str, Any]) -> dict[str, Any]:
    value = json_value(payload.get("arguments", payload.get("input", payload.get("action"))))
    if isinstance(value, dict):
        return value

    return {} if value is None else {"value": value}


def json_value(value: Any) -> Any:
    if not isinstance(value, str):
        return value

    return json_load(value, value)


def output_text(value: Any) -> str | None:
    if value is None:
        return None

    return value if isinstance(value, str) else json_dump(value)


def is_error(value: Any) -> bool:
    if not isinstance(value, dict):
        return False

    status = value.get("status")
    return (
        value.get("is_error") is True
        or value.get("success") is False
        or isinstance(status, str)
        and status in {"failed", "error"}
    )


def integer(value: Any) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None
