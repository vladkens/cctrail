from collections.abc import Iterable, Iterator
from dataclasses import dataclass
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Literal

from .models import Message, Session, ToolCall, Usage
from .utils import (
    WorkerPool,
    data_paths,
    extract_text,
    history_roots,
    int_or_zero,
    json_lines,
    json_load,
    parse_timestamp,
    string_or_none,
)


@dataclass(frozen=True)
class ParseJob:
    path: Path
    session_id: str
    kind: Literal["main", "agent"]


def default_roots() -> list[Path]:
    home = Path.home()
    return data_paths(
        "CLAUDE_CONFIG_DIR",
        (home / ".config" / "claude", home / ".claude"),
    )


def iter_sessions(
    root: str | Path | None = None,
    *,
    roots: Iterable[str | Path] | None = None,
) -> Iterator[Session]:
    """Stream Claude Code sessions, one file at a time.

    Without an explicit root, `CLAUDE_CONFIG_DIR` accepts comma-separated
    directories and replaces the default `~/.config/claude` and `~/.claude`
    lookup. Sub-agent transcripts are attached to the tool call that launched
    them, so a fully parsed `Session` already contains its whole agent tree.
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
    paths = sorted({path for search_root in search_roots for path in search_root.rglob("*.jsonl")})
    agent_paths = subagent_paths(paths)
    jobs = [
        ParseJob(
            path=path,
            session_id=path.stem.removeprefix("agent-"),
            kind="agent" if "subagents" in path.parts else "main",
        )
        for path in paths
    ]
    parsed = dict(pool.map(parse_job, jobs))
    linked_subagents: set[Path] = set()
    for path in paths:
        if "subagents" in path.parts:
            continue

        session = parsed[path]
        link_subagents(
            session,
            path.parent / path.stem,
            agent_paths,
            parsed,
            linked_subagents,
        )
        if session.messages:
            yield session

    for path in paths:
        if "subagents" not in path.parts or path in linked_subagents:
            continue

        session = parsed[path]
        link_subagents(session, session_dir(path), agent_paths, parsed, linked_subagents)
        if session.messages:
            yield session


def parse_job(job: ParseJob) -> tuple[Path, Session]:
    return job.path, parse_file(job.path, session_id=job.session_id, kind=job.kind)


def link_subagents(
    session: Session,
    parent_dir: Path,
    agent_paths: dict[tuple[Path, str], Path],
    parsed: dict[Path, Session],
    linked: set[Path],
) -> None:
    for call in session.iter_tool_calls():
        if call.name not in ("Agent", "Task"):
            continue

        raw = call.raw_result if isinstance(call.raw_result, dict) else {}
        agent_id = string_or_none(raw.get("agentId"))
        sub_path = agent_paths.get((parent_dir, agent_id or call.id))
        if sub_path is None or sub_path not in parsed:
            continue

        agent = parsed[sub_path]
        call.agent = apply_agent_metadata(agent, raw, call, session)
        if sub_path in linked:
            continue
        linked.add(sub_path)
        link_subagents(agent, parent_dir, agent_paths, parsed, linked)


def subagent_paths(paths: list[Path]) -> dict[tuple[Path, str], Path]:
    result: dict[tuple[Path, str], Path] = {}
    for path in paths:
        if "subagents" not in path.parts or not path.name.startswith("agent-"):
            continue

        parent_dir = session_dir(path)
        result[parent_dir, path.stem.removeprefix("agent-")] = path
        tool_id = string_or_none(read_object(path.with_suffix(".meta.json")).get("toolUseId"))
        if tool_id:
            result[parent_dir, tool_id] = path

    return result


def session_dir(path: Path) -> Path:
    return next(parent.parent for parent in path.parents if parent.name == "subagents")


def usage_from_raw(raw: Any) -> Usage:
    if not isinstance(raw, dict):
        return Usage()

    cache_write = int_or_zero(raw.get("cache_creation_input_tokens"))
    cache_creation = raw.get("cache_creation")
    if isinstance(cache_creation, dict):
        cache_write_5m = int_or_zero(cache_creation.get("ephemeral_5m_input_tokens"))
        cache_write_1h = int_or_zero(cache_creation.get("ephemeral_1h_input_tokens"))
    else:
        cache_write_5m = cache_write
        cache_write_1h = 0

    result = Usage(
        input=int_or_zero(raw.get("input_tokens")),
        cache_write_5m=cache_write_5m,
        cache_write_1h=cache_write_1h,
        cache_read=int_or_zero(raw.get("cache_read_input_tokens")),
        output=int_or_zero(raw.get("output_tokens")),
    )
    iterations = raw.get("iterations")
    if isinstance(iterations, list):
        for iteration in iterations:
            if isinstance(iteration, dict) and iteration.get("type") == "advisor_message":
                result += usage_from_raw(iteration)
    return result


def speed_from_raw(raw: Any) -> Literal["standard", "fast"] | None:
    if not isinstance(raw, dict):
        return None

    speed = string_or_none(raw.get("speed")) or string_or_none(raw.get("service_tier"))
    if speed in ("fast", "priority"):
        return "fast"
    if speed in ("standard", "default"):
        return "standard"
    return None


def parse_file(
    path: Path,
    *,
    session_id: str | None = None,
    kind: Literal["main", "agent"] = "main",
) -> Session:
    return ClaudeFileParser(path, session_id=session_id, kind=kind).parse()


class ClaudeFileParser:
    def __init__(
        self,
        path: Path,
        *,
        session_id: str | None,
        kind: Literal["main", "agent"],
    ) -> None:
        self.path = path
        self.kind = kind
        self.session = Session(id=session_id or path.stem, kind=kind)
        self.pending_tools: dict[str, ToolCall] = {}
        self.open_turns: dict[str | tuple[str, str | None], Message] = {}
        self.last_user_message: Message | None = None
        self.ai_titles: dict[str, str] = {}
        self.custom_titles: dict[str, str] = {}

        if kind == "agent":
            metadata = read_object(path.with_suffix(".meta.json"))
            self.session.agent_type = string_or_none(metadata.get("agentType"))
            self.session.title = (
                string_or_none(metadata.get("description")) or self.session.agent_type
            )

    def parse(self) -> Session:
        for item in json_lines(self.path):
            self.parse_item(item)

        if self.kind == "main":
            self.session.title = self.custom_titles.get(
                self.session.id,
                self.ai_titles.get(self.session.id),
            )
        return self.session

    def parse_item(self, item: dict[str, Any]) -> None:
        cwd = string_or_none(item.get("cwd"))
        if cwd and self.session.cwd is None:
            self.session.cwd = cwd

        branch = string_or_none(item.get("gitBranch"))
        if branch and self.session.git_branch is None:
            self.session.git_branch = branch

        # Sub-agent transcripts carry the *parent* sessionId on every line, so
        # only trust this field to identify main sessions (which may resume
        # under a new id after a compaction).
        if self.kind == "main":
            sid = string_or_none(item.get("sessionId"))
            if sid:
                self.session.id = sid

        item_type = item.get("type")
        if item_type in ("ai-title", "custom-title"):
            self.parse_title(item, item_type)
        elif item_type == "system":
            self.parse_system(item)
        elif item_type == "assistant":
            self.parse_assistant(item)
        elif item_type == "user":
            self.parse_user(item)

    def parse_title(self, item: dict[str, Any], item_type: str) -> None:
        sid = string_or_none(item.get("sessionId"))
        if item_type == "ai-title":
            title = string_or_none(item.get("aiTitle"))
            titles = self.ai_titles
        else:
            title = string_or_none(item.get("customTitle"))
            titles = self.custom_titles
        if sid and title:
            titles[sid] = title

    def parse_system(self, item: dict[str, Any]) -> None:
        if item.get("subtype") != "turn_duration" or self.last_user_message is None:
            return
        duration_ms = item.get("durationMs")
        if isinstance(duration_ms, (int, float)):
            self.last_user_message.turn_duration = timedelta(milliseconds=duration_ms)

    def parse_assistant(self, item: dict[str, Any]) -> None:
        message = item.get("message")
        if not isinstance(message, dict):
            return

        timestamp = parse_timestamp(item.get("timestamp"))
        message_id = string_or_none(message.get("id"))
        request_id = string_or_none(item.get("requestId"))
        message_key: str | tuple[str, str | None] | None
        if message_id is not None:
            message_key = message_id, request_id
        else:
            message_key = request_id or string_or_none(item.get("uuid"))

        turn = self.open_turns.get(message_key) if message_key is not None else None
        if turn is None:
            turn = Message(
                role="assistant",
                timestamp=timestamp,
                id=message_id,
                request_id=request_id,
                is_sidechain=item.get("isSidechain") is True,
            )
            if message_key is not None:
                self.open_turns[message_key] = turn
            self.session.messages.append(turn)

        turn.model = turn.model or string_or_none(message.get("model"))
        turn.is_api_error = turn.is_api_error or item.get("isApiErrorMessage") is True
        raw_usage = message.get("usage")
        usage = usage_from_raw(raw_usage)
        if turn.usage is None or usage.total > turn.usage.total:
            turn.usage = usage
        turn.speed = speed_from_raw(raw_usage) or turn.speed

        content = message.get("content")
        if not isinstance(content, list):
            return
        for block in content:
            if isinstance(block, dict):
                self.parse_assistant_block(block, turn, timestamp)

    def parse_assistant_block(
        self,
        block: dict[str, Any],
        turn: Message,
        timestamp: datetime | None,
    ) -> None:
        block_type = block.get("type")
        if block_type == "text":
            turn.text += string_or_none(block.get("text")) or ""
            return
        if block_type != "tool_use":
            return

        call_id = string_or_none(block.get("id"))
        name = string_or_none(block.get("name"))
        if call_id is None or name is None:
            return

        call_input = block.get("input")
        call = ToolCall(
            id=call_id,
            name=name,
            input=call_input if isinstance(call_input, dict) else {},
            started_at=timestamp,
        )
        turn.tool_calls.append(call)
        self.pending_tools[call.id] = call

    def parse_user(self, item: dict[str, Any]) -> None:
        message = item.get("message")
        if not isinstance(message, dict):
            return

        content = message.get("content")
        timestamp = parse_timestamp(item.get("timestamp"))
        blocks = content if isinstance(content, list) else []
        tool_results = [
            block
            for block in blocks
            if isinstance(block, dict) and block.get("type") == "tool_result"
        ]
        if tool_results:
            for block in tool_results:
                self.parse_tool_result(block, item, timestamp)
            return

        # Synthetic prompts injected by the harness itself (hook output,
        # local-command caveats, skill listings) aren't user input.
        if item.get("isMeta"):
            return

        text = extract_text(content, ("text", "input_text")).strip()
        if not text:
            return

        user_message = Message(role="user", timestamp=timestamp, text=text)
        self.session.messages.append(user_message)
        self.last_user_message = user_message

    def parse_tool_result(
        self,
        block: dict[str, Any],
        item: dict[str, Any],
        timestamp: datetime | None,
    ) -> None:
        tool_id = string_or_none(block.get("tool_use_id"))
        if tool_id is None:
            return
        call = self.pending_tools.pop(tool_id, None)
        if call is None:
            return

        call.ended_at = timestamp
        call.is_error = block.get("is_error") is True
        call.output_text = extract_text(block.get("content"), ("text", "input_text"))
        call.raw_result = item.get("toolUseResult")
        if call.name in ("Agent", "Task"):
            call.agent = build_agent_session(call.raw_result, call, self.session)


def build_agent_session(
    raw: Any,
    call: ToolCall,
    parent: Session,
) -> Session | None:
    raw = raw if isinstance(raw, dict) else {}
    agent_id = string_or_none(raw.get("agentId"))
    if agent_id is None:
        return None

    agent = Session(id=agent_id, kind="agent")
    return apply_agent_metadata(agent, raw, call, parent)


def apply_agent_metadata(
    agent: Session,
    raw: dict[str, Any],
    call: ToolCall,
    parent: Session,
) -> Session:
    agent.cwd = agent.cwd or parent.cwd
    agent.git_branch = agent.git_branch or parent.git_branch
    agent.agent_type = (
        string_or_none(raw.get("agentType"))
        or string_or_none(call.input.get("subagent_type"))
        or agent.agent_type
    )
    agent.title = string_or_none(call.input.get("description")) or agent.title or agent.agent_type

    # Older/aborted runs may lack a resolvable transcript file; retain the
    # final usage snapshot Claude Code attached to the tool result.
    if not agent.messages:
        raw_usage = raw.get("usage")
        agent.messages.append(
            Message(
                role="assistant",
                timestamp=call.ended_at,
                usage=usage_from_raw(raw_usage),
                speed=speed_from_raw(raw_usage),
            )
        )

    duration_ms = raw.get("totalDurationMs")
    if isinstance(duration_ms, (int, float)):
        agent.duration_override = timedelta(milliseconds=duration_ms)

    return agent


def read_object(path: Path) -> dict[str, Any]:
    try:
        value = json_load(path.read_text(encoding="utf-8"))
    except OSError:
        return {}

    return value if isinstance(value, dict) else {}
