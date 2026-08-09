from collections.abc import Iterable, Iterator
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any, Literal

from .models import Message, Session, ToolCall, Usage
from .utils import (
    WorkerPool,
    data_paths,
    extract_text,
    history_roots,
    int_or_zero,
    json_dump,
    json_lines,
    json_load,
    parse_timestamp,
    string_or_none,
)


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


@dataclass(frozen=True)
class ParseJob:
    rollout: Rollout
    title: str | None
    replay_end: int


def default_roots() -> list[Path]:
    return data_paths("CODEX_HOME", (Path.home() / ".codex",))


def iter_sessions(
    root: str | Path | None = None,
    *,
    roots: Iterable[str | Path] | None = None,
) -> Iterator[Session]:
    """Stream Codex sessions, attaching spawned agents to their tool calls.

    Without an explicit root, `CODEX_HOME` accepts comma-separated directories
    and replaces the default `~/.codex` lookup.
    """
    with WorkerPool() as pool:
        yield from iter_sessions_with_pool(pool, root=root, roots=roots)


def iter_sessions_with_pool(
    pool: WorkerPool,
    *,
    root: str | Path | None = None,
    roots: Iterable[str | Path] | None = None,
) -> Iterator[Session]:
    search_roots = history_roots(root, roots, default_roots())
    titles = {
        session_id: title
        for search_root in search_roots
        for session_id, title in load_titles(search_root).items()
    }
    rollouts = []
    for search_root in search_roots:
        for path in history_files(search_root):
            rollout = read_meta(path)
            if rollout is not None:
                rollouts.append(rollout)
    by_id = {rollout.id: rollout for rollout in rollouts}
    rollouts = list(by_id.values())
    paths = replay_paths(rollouts, by_id)

    replay_events = dict(zip(paths, pool.map(usage_events, paths)))
    jobs = parse_jobs(rollouts, by_id, titles, replay_events)
    parsed = pool.map(parse_job, jobs)

    sessions = {session.id: session for session in parsed}

    attached: set[str] = set()
    for rollout in by_id.values():
        if rollout.parent_id is None:
            continue
        parent = sessions.get(rollout.parent_id)
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
) -> list[ParseJob]:
    return [
        ParseJob(
            rollout=rollout,
            title=titles.get(rollout.id),
            replay_end=replay_end(rollout, by_id, replay_events),
        )
        for rollout in rollouts
    ]


def parse_job(job: ParseJob) -> Session:
    return parse_file(job.rollout, job.title, job.replay_end)


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
    return CodexFileParser(rollout, title=title, replay_end=replay_end).parse()


class CodexFileParser:
    def __init__(self, rollout: Rollout, *, title: str | None, replay_end: int) -> None:
        self.rollout = rollout
        self.replay_end = replay_end
        self.session = Session(
            id=rollout.id,
            kind="agent" if rollout.parent_id else "main",
            provider="codex",
            agent_type=rollout.agent_type,
            title=title or rollout.agent_name or rollout.agent_type,
            cwd=rollout.cwd,
            git_branch=rollout.git_branch,
        )
        self.pending_tools: dict[str, ToolCall] = {}
        self.current_assistant: Message | None = None
        self.current_user: Message | None = None
        self.current_model: str | None = None
        self.current_speed: Literal["standard", "fast"] | None = None
        self.turn_started_at: datetime | None = None
        self.last_cumulative: tuple[int, ...] | None = None

    def parse(self) -> Session:
        for line_number, item in enumerate(json_lines(self.rollout.path)):
            self.parse_item(line_number, item)
        return self.session

    def assistant(self, timestamp: datetime | None) -> Message:
        if self.current_assistant is None:
            self.current_assistant = Message(
                role="assistant",
                timestamp=timestamp,
                model=self.current_model,
                speed=self.current_speed,
            )
            self.session.messages.append(self.current_assistant)
        elif self.current_assistant.model is None:
            self.current_assistant.model = self.current_model
        return self.current_assistant

    def parse_item(self, line_number: int, item: dict[str, Any]) -> None:
        payload = item.get("payload")
        if not isinstance(payload, dict):
            return

        timestamp = parse_timestamp(item.get("timestamp"))
        item_type = item.get("type")
        if item_type == "session_meta":
            return

        ordinal = integer(item.get("ordinal"))
        inherited = line_number <= self.replay_end or (
            self.rollout.own_start_ordinal is not None
            and ordinal is not None
            and ordinal < self.rollout.own_start_ordinal
        )
        if inherited:
            self.parse_inherited(item_type, payload)
            return

        if item_type == "turn_context":
            self.parse_turn_context(payload)
        elif item_type == "event_msg":
            self.parse_event(payload, timestamp)
        elif item_type == "response_item":
            self.parse_response(payload, timestamp)

    def parse_inherited(self, item_type: Any, payload: dict[str, Any]) -> None:
        if item_type == "turn_context":
            self.current_model = string_or_none(payload.get("model")) or self.current_model
            return
        if item_type != "event_msg":
            return

        subtype = payload.get("type")
        if subtype == "thread_settings_applied":
            self.update_speed(payload)
        elif subtype == "token_count":
            info = payload.get("info")
            if isinstance(info, dict):
                cumulative = usage_signature(info.get("total_token_usage"))
                if cumulative is not None:
                    self.last_cumulative = cumulative

    def parse_turn_context(self, payload: dict[str, Any]) -> None:
        self.current_model = string_or_none(payload.get("model")) or self.current_model
        self.update_speed(payload)
        self.session.cwd = string_or_none(payload.get("cwd")) or self.session.cwd

    def update_speed(self, payload: dict[str, Any]) -> None:
        has_speed, speed = speed_from_payload(payload)
        if has_speed:
            self.current_speed = speed

    def parse_event(self, payload: dict[str, Any], timestamp: datetime | None) -> None:
        subtype = payload.get("type")
        if subtype == "task_started":
            self.current_user = None
            self.turn_started_at = parse_timestamp(payload.get("started_at")) or timestamp
        elif subtype == "user_message":
            text = string_or_none(payload.get("message"))
            if text:
                self.current_user = Message(role="user", timestamp=timestamp, text=text)
                self.session.messages.append(self.current_user)
            self.current_assistant = None
        elif subtype == "task_complete":
            if self.current_user and self.turn_started_at and timestamp:
                self.current_user.turn_duration = timestamp - self.turn_started_at
            self.current_assistant = None
            self.turn_started_at = None
        elif subtype == "thread_settings_applied":
            self.update_speed(payload)
        elif subtype == "token_count":
            self.parse_usage(payload, timestamp)

    def parse_usage(self, payload: dict[str, Any], timestamp: datetime | None) -> None:
        info = payload.get("info")
        if not isinstance(info, dict):
            return

        cumulative = usage_signature(info.get("total_token_usage"))
        if cumulative is not None and cumulative == self.last_cumulative:
            return

        self.last_cumulative = cumulative
        message_usage = usage_from_raw(info.get("last_token_usage"))
        if message_usage is not None:
            message = self.assistant(timestamp)
            message.usage = message_usage
            message.timestamp = timestamp or message.timestamp
            message.speed = self.current_speed
        self.current_assistant = None

    def parse_response(self, payload: dict[str, Any], timestamp: datetime | None) -> None:
        subtype = payload.get("type")
        if subtype == "message" and payload.get("role") == "assistant":
            text = extract_text(payload.get("content"), ("output_text", "text"))
            if text:
                message = self.assistant(timestamp)
                message.text = "\n".join(filter(None, (message.text, text)))
        elif subtype in {"function_call", "custom_tool_call", "tool_search_call"}:
            call_id = tool_call_id(payload)
            call = ToolCall(
                id=call_id,
                name=string_or_none(payload.get("name")) or subtype.removesuffix("_call"),
                input=tool_input(payload),
                started_at=timestamp,
            )
            self.assistant(timestamp).tool_calls.append(call)
            self.pending_tools[call_id] = call
        elif subtype in {"function_call_output", "custom_tool_call_output", "tool_search_output"}:
            call = self.pending_tools.get(tool_call_id(payload))
            if call is not None:
                output = payload.get("output")
                call.ended_at = timestamp
                call.output_text = output_text(output)
                call.raw_result = json_value(output)
                call.is_error = is_error(call.raw_result)
        elif subtype in {"web_search_call", "image_generation_call"}:
            status = payload.get("status")
            self.assistant(timestamp).tool_calls.append(
                ToolCall(
                    id=tool_call_id(payload),
                    name=subtype.removesuffix("_call"),
                    input=tool_input(payload),
                    started_at=timestamp,
                    ended_at=timestamp if status == "completed" else None,
                    raw_result=payload,
                    is_error=status == "failed",
                )
            )


def tool_call_id(payload: dict[str, Any]) -> str:
    return string_or_none(payload.get("call_id")) or string_or_none(payload.get("id")) or ""


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
        cache_write_5m=int_or_zero(value.get("cache_write_input_tokens")),
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


def speed_from_payload(
    payload: dict[str, Any],
) -> tuple[bool, Literal["standard", "fast"] | None]:
    settings = payload.get("thread_settings")
    source = settings if isinstance(settings, dict) else payload
    if "service_tier" not in source:
        return False, None

    speed = string_or_none(source.get("service_tier"))
    if speed in ("fast", "priority"):
        return True, "fast"
    if speed in ("standard", "default"):
        return True, "standard"
    return True, None


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
