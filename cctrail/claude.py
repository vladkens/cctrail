import json
from collections.abc import Iterator
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Literal

from .models import Message, Session, ToolCall, Usage


def default_root() -> Path:
    return Path.home() / ".claude" / "projects"


def iter_sessions(root: str | Path | None = None) -> Iterator[Session]:
    """Stream Claude Code sessions found under `root`, one file at a time.

    Defaults to `~/.claude/projects`. Sub-agent transcripts (spawned via the
    `Agent`/`Task` tool) are resolved automatically and attached to the tool
    call that launched them, so a fully parsed `Session` already contains its
    whole agent tree.
    """
    root = Path(root) if root is not None else default_root()
    for path in sorted(root.rglob("*.jsonl")):
        if "subagents" in path.parts:
            continue
        session = parse_file(path)
        if session.messages:
            yield session


def read_jsonl(path: Path) -> Iterator[dict[str, Any]]:
    with path.open("r", encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(value, dict):
                yield value


def parse_timestamp(value: Any) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def extract_text(content: Any) -> str:
    if isinstance(content, str):
        return content
    if not isinstance(content, list):
        return ""
    parts = []
    for block in content:
        if not isinstance(block, dict) or block.get("type") not in ("text", "input_text"):
            continue
        text = block.get("text")
        if isinstance(text, str):
            parts.append(text)
    return "\n".join(parts)


def usage_from_raw(raw: Any) -> Usage:
    if not isinstance(raw, dict):
        return Usage()
    return Usage(
        input=int(raw.get("input_tokens") or 0),
        cache_write=int(raw.get("cache_creation_input_tokens") or 0),
        cache_read=int(raw.get("cache_read_input_tokens") or 0),
        output=int(raw.get("output_tokens") or 0),
    )


def parse_file(
    path: Path, *, session_id: str | None = None, kind: Literal["main", "agent"] = "main"
) -> Session:
    session = Session(id=session_id or path.stem, kind=kind)
    pending_tools: dict[str, ToolCall] = {}
    open_turns: dict[Any, Message] = {}
    last_user_message: Message | None = None

    for item in read_jsonl(path):
        cwd = item.get("cwd")
        if cwd and not session.cwd:
            session.cwd = cwd
        branch = item.get("gitBranch")
        if branch and not session.git_branch:
            session.git_branch = branch
        # Sub-agent transcripts carry the *parent* sessionId on every line, so
        # only trust this field to identify main sessions (which may resume
        # under a new id after a compaction).
        if kind == "main":
            sid = item.get("sessionId")
            if isinstance(sid, str):
                session.id = sid

        item_type = item.get("type")

        if item_type == "ai-title":
            title = item.get("aiTitle")
            if title:
                session.title = title
            continue

        if item_type == "system":
            if item.get("subtype") == "turn_duration" and last_user_message is not None:
                duration_ms = item.get("durationMs")
                if isinstance(duration_ms, (int, float)):
                    last_user_message.turn_duration = timedelta(milliseconds=duration_ms)
            continue

        if item_type == "assistant":
            message = item.get("message") or {}
            content = message.get("content") or []
            timestamp = parse_timestamp(item.get("timestamp"))
            # Claude Code writes one JSONL line per content block but repeats
            # the same message id/usage on each, so blocks are merged back
            # into a single logical turn here.
            msg_key = message.get("id") or item.get("requestId") or item.get("uuid")
            turn = open_turns.get(msg_key)
            if turn is None:
                turn = Message(role="assistant", timestamp=timestamp)
                open_turns[msg_key] = turn
                session.messages.append(turn)
            turn.model = turn.model or message.get("model")
            turn.is_api_error = turn.is_api_error or bool(item.get("isApiErrorMessage"))
            usage = usage_from_raw(message.get("usage"))
            if turn.usage is None or usage.total > turn.usage.total:
                turn.usage = usage
            for block in content:
                if not isinstance(block, dict):
                    continue
                block_type = block.get("type")
                if block_type == "text":
                    turn.text += block.get("text", "")
                elif block_type == "tool_use":
                    call = ToolCall(
                        id=block.get("id", ""),
                        name=block.get("name", ""),
                        input=block.get("input") or {},
                        started_at=timestamp,
                    )
                    turn.tool_calls.append(call)
                    pending_tools[call.id] = call
            continue

        if item_type == "user":
            message = item.get("message") or {}
            content = message.get("content")
            timestamp = parse_timestamp(item.get("timestamp"))
            blocks = content if isinstance(content, list) else []
            tool_results = [
                b for b in blocks if isinstance(b, dict) and b.get("type") == "tool_result"
            ]

            if tool_results:
                for block in tool_results:
                    call = pending_tools.pop(block.get("tool_use_id"), None)
                    if call is None:
                        continue
                    call.ended_at = timestamp
                    call.is_error = bool(block.get("is_error"))
                    call.output_text = extract_text(block.get("content"))
                    call.raw_result = item.get("toolUseResult")
                    if call.name in ("Agent", "Task"):
                        call.agent = build_agent_session(path, call.raw_result, call, session)
                continue

            # Synthetic prompts injected by the harness itself (hook output,
            # local-command caveats, skill listings) aren't user input.
            if item.get("isMeta"):
                continue
            text = extract_text(content).strip()
            if not text:
                continue
            message_obj = Message(role="user", timestamp=timestamp, text=text)
            session.messages.append(message_obj)
            last_user_message = message_obj
            continue

    return session


def build_agent_session(
    parent_path: Path, raw: Any, call: ToolCall, parent: Session
) -> Session | None:
    if not isinstance(raw, dict):
        return None
    agent_id = raw.get("agentId")
    if not isinstance(agent_id, str):
        return None

    sub_path = parent_path.parent / parent_path.stem / "subagents" / f"agent-{agent_id}.jsonl"
    if sub_path.exists():
        agent = parse_file(sub_path, session_id=agent_id, kind="agent")
    else:
        agent = Session(id=agent_id, kind="agent")

    agent.cwd = agent.cwd or parent.cwd
    agent.git_branch = agent.git_branch or parent.git_branch
    agent.agent_type = raw.get("agentType") or call.input.get("subagent_type")
    agent.title = call.input.get("description") or agent.agent_type

    # Older/aborted runs may lack a resolvable transcript file; fall back to
    # the aggregate usage Claude Code already attached to the tool result.
    if not agent.messages:
        usage = usage_from_raw(raw.get("usage"))
        agent.messages.append(Message(role="assistant", timestamp=call.ended_at, usage=usage))

    duration_ms = raw.get("totalDurationMs")
    if isinstance(duration_ms, (int, float)):
        agent.duration_override = timedelta(milliseconds=duration_ms)

    return agent
