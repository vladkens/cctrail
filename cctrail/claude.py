from collections.abc import Iterable, Iterator
from datetime import timedelta
from pathlib import Path
from typing import Any, Literal

from .models import Message, Session, ToolCall, Usage
from .utils import (
    data_paths,
    extract_text,
    int_or_zero,
    json_lines,
    json_load,
    parse_timestamp,
    string_or_none,
)


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
    if root is not None and roots is not None:
        raise ValueError("root and roots cannot be combined")
    search_roots = (
        [Path(item).expanduser() for item in roots]
        if roots is not None
        else [Path(root).expanduser()]
        if root is not None
        else default_roots()
    )
    paths = sorted({path for search_root in search_roots for path in search_root.rglob("*.jsonl")})
    agent_paths = subagent_paths(paths)
    linked_subagents: set[Path] = set()
    for path in paths:
        if "subagents" in path.parts:
            continue

        session = parse_file(
            path,
            subagent_paths=agent_paths,
            linked_subagents=linked_subagents,
            session_dir=path.parent / path.stem,
        )
        if session.messages:
            yield session

    for path in paths:
        if "subagents" not in path.parts or path in linked_subagents:
            continue

        session = parse_file(
            path,
            session_id=path.stem.removeprefix("agent-"),
            kind="agent",
            subagent_paths=agent_paths,
            linked_subagents=linked_subagents,
            session_dir=session_dir(path),
        )
        if session.messages:
            yield session


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
    subagent_paths: dict[tuple[Path, str], Path] | None = None,
    linked_subagents: set[Path] | None = None,
    session_dir: Path | None = None,
) -> Session:
    session = Session(id=session_id or path.stem, kind=kind)
    if kind == "agent":
        metadata = read_object(path.with_suffix(".meta.json"))
        session.agent_type = string_or_none(metadata.get("agentType"))
        session.title = string_or_none(metadata.get("description")) or session.agent_type

    pending_tools: dict[str, ToolCall] = {}
    open_turns: dict[str | tuple[str, str | None], Message] = {}
    last_user_message: Message | None = None
    ai_titles: dict[str, str] = {}
    custom_titles: dict[str, str] = {}

    for item in json_lines(path):
        cwd = string_or_none(item.get("cwd"))
        if cwd and session.cwd is None:
            session.cwd = cwd

        branch = string_or_none(item.get("gitBranch"))
        if branch and session.git_branch is None:
            session.git_branch = branch

        # Sub-agent transcripts carry the *parent* sessionId on every line, so
        # only trust this field to identify main sessions (which may resume
        # under a new id after a compaction).
        if kind == "main":
            sid = string_or_none(item.get("sessionId"))
            if sid:
                session.id = sid

        item_type = item.get("type")

        if item_type in ("ai-title", "custom-title"):
            sid = string_or_none(item.get("sessionId"))
            field = "aiTitle" if item_type == "ai-title" else "customTitle"
            title = string_or_none(item.get(field))
            if sid and title:
                (ai_titles if item_type == "ai-title" else custom_titles)[sid] = title
            continue

        if item_type == "system":
            if item.get("subtype") == "turn_duration" and last_user_message is not None:
                duration_ms = item.get("durationMs")
                if isinstance(duration_ms, (int, float)):
                    last_user_message.turn_duration = timedelta(milliseconds=duration_ms)
            continue

        if item_type == "assistant":
            message = item.get("message")
            if not isinstance(message, dict):
                continue

            content = message.get("content")
            if not isinstance(content, list):
                content = []
            timestamp = parse_timestamp(item.get("timestamp"))
            # Claude Code writes one JSONL line per content block but repeats
            # the same message id/usage on each, so blocks are merged back
            # into a single logical turn here.
            message_id = string_or_none(message.get("id"))
            request_id = string_or_none(item.get("requestId"))

            uuid = string_or_none(item.get("uuid"))
            msg_key = (message_id, request_id) if message_id else request_id or uuid
            turn = open_turns.get(msg_key) if msg_key is not None else None
            if turn is None:
                turn = Message(
                    role="assistant",
                    timestamp=timestamp,
                    id=message_id,
                    request_id=request_id,
                    is_sidechain=item.get("isSidechain") is True,
                )
                if msg_key is not None:
                    open_turns[msg_key] = turn
                session.messages.append(turn)

            turn.model = turn.model or string_or_none(message.get("model"))
            turn.is_api_error = turn.is_api_error or item.get("isApiErrorMessage") is True
            raw_usage = message.get("usage")
            usage = usage_from_raw(raw_usage)
            if turn.usage is None or usage.total > turn.usage.total:
                turn.usage = usage
            turn.speed = speed_from_raw(raw_usage) or turn.speed

            for block in content:
                if not isinstance(block, dict):
                    continue

                block_type = block.get("type")
                if block_type == "text":
                    turn.text += string_or_none(block.get("text")) or ""
                elif block_type == "tool_use":
                    call_id = string_or_none(block.get("id"))
                    name = string_or_none(block.get("name"))
                    if call_id is None or name is None:
                        continue

                    call_input = block.get("input")
                    call = ToolCall(
                        id=call_id,
                        name=name,
                        input=call_input if isinstance(call_input, dict) else {},
                        started_at=timestamp,
                    )
                    turn.tool_calls.append(call)
                    pending_tools[call.id] = call
            continue

        if item_type == "user":
            message = item.get("message")
            if not isinstance(message, dict):
                continue

            content = message.get("content")
            timestamp = parse_timestamp(item.get("timestamp"))
            blocks = content if isinstance(content, list) else []
            tool_results = [
                b for b in blocks if isinstance(b, dict) and b.get("type") == "tool_result"
            ]

            if tool_results:
                for block in tool_results:
                    tool_id = string_or_none(block.get("tool_use_id"))
                    if tool_id is None:
                        continue

                    call = pending_tools.pop(tool_id, None)
                    if call is None:
                        continue

                    call.ended_at = timestamp
                    call.is_error = block.get("is_error") is True
                    call.output_text = extract_text(block.get("content"), ("text", "input_text"))
                    call.raw_result = item.get("toolUseResult")
                    if call.name in ("Agent", "Task"):
                        call.agent = build_agent_session(
                            call.raw_result,
                            call,
                            session,
                            subagent_paths,
                            linked_subagents,
                            session_dir,
                        )
                continue

            # Synthetic prompts injected by the harness itself (hook output,
            # local-command caveats, skill listings) aren't user input.
            if item.get("isMeta"):
                continue

            text = extract_text(content, ("text", "input_text")).strip()
            if not text:
                continue

            message_obj = Message(role="user", timestamp=timestamp, text=text)
            session.messages.append(message_obj)
            last_user_message = message_obj
            continue

    if kind == "main":
        if session.id in custom_titles:
            session.title = custom_titles[session.id]
        else:
            session.title = ai_titles.get(session.id)

    return session


def build_agent_session(
    raw: Any,
    call: ToolCall,
    parent: Session,
    subagent_paths: dict[tuple[Path, str], Path] | None,
    linked_subagents: set[Path] | None,
    session_dir: Path | None,
) -> Session | None:
    raw = raw if isinstance(raw, dict) else {}
    agent_id = string_or_none(raw.get("agentId"))

    sub_path = (
        subagent_paths.get((session_dir, agent_id or call.id))
        if subagent_paths and session_dir
        else None
    )
    if agent_id is None and sub_path is not None:
        agent_id = sub_path.stem.removeprefix("agent-")

    if agent_id is None:
        return None

    if sub_path is not None:
        if linked_subagents is not None:
            linked_subagents.add(sub_path)
        agent = parse_file(
            sub_path,
            session_id=agent_id,
            kind="agent",
            subagent_paths=subagent_paths,
            linked_subagents=linked_subagents,
            session_dir=session_dir,
        )
    else:
        agent = Session(id=agent_id, kind="agent")

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
