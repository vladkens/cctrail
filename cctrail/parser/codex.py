from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import orjson

from ..model import Event, Provider, Session, Speed, Usage
from . import AgentCall, inherit_roots, jsonl, jsonl_files

Signature = tuple[int, int, int, int, int, int]


@dataclass
class Rollout:
    path: Path
    id: str
    parent_id: str | None
    forked_from_id: str | None
    has_history_base: bool
    workdir: str
    own_start: int | None


@dataclass(frozen=True)
class RawUsage:
    input_tokens: int = 0
    cached_input_tokens: int = 0
    cache_write_input_tokens: int = 0
    output_tokens: int = 0
    reasoning_output_tokens: int = 0
    total_tokens: int = 0


@dataclass
class Record:
    event: Event | None = None
    signature: Signature | None = None


@dataclass
class Parsed:
    session: Session
    calls: list[AgentCall] = field(default_factory=list)
    records: list[Record] = field(default_factory=list)
    signatures: list[Signature] = field(default_factory=list)


def parse(root: Path) -> list[Session]:
    rollouts: list[Rollout] = []
    indexes: dict[str, int] = {}
    for path in history_files(root):
        rollout = read_metadata(path)
        if rollout is None:
            continue
        if rollout.id in indexes:
            rollouts[indexes[rollout.id]] = rollout
        else:
            indexes[rollout.id] = len(rollouts)
            rollouts.append(rollout)

    parsed = [parse_file(x) for x in rollouts]
    signatures = {rollout.id: parsed[i].signatures for i, rollout in enumerate(rollouts)}
    for i, item in enumerate(parsed):
        rollout = rollouts[i]
        replay = None
        if not rollout.has_history_base and rollout.own_start is None and rollout.forked_from_id:
            replay = signatures.get(rollout.forked_from_id)
        item.session.events = (
            strip_replay(item.records, replay)
            if replay is not None
            else [x.event for x in item.records if x.event is not None]
        )

    parents: dict[int, int] = {}
    for rollout in rollouts:
        if rollout.parent_id is None:
            continue
        child = indexes.get(rollout.id)
        parent = indexes.get(rollout.parent_id)
        if child is None or parent is None:
            continue
        calls = parsed[parent].calls
        call = next(
            (i for i, x in enumerate(calls) if not x.resolved and x.agent_id == rollout.id),
            None,
        )
        if call is None:
            call = next((i for i, x in enumerate(calls) if not x.resolved), None)
        if call is not None:
            calls[call].resolved = True
            parents[child] = parent

    sessions = [x.session for x in parsed]
    inherit_roots(sessions, parents)
    return [x for x in sessions if x.events]


def history_files(root: Path) -> list[Path]:
    roots = [root / "archived_sessions", root / "sessions"]
    return jsonl_files(roots if any(x.is_dir() for x in roots) else [root])


def read_metadata(path: Path) -> Rollout | None:
    value = next((x for x in jsonl(path) if x.get("type") == "session_meta"), None)
    payload = value.get("payload") if value else None
    if not isinstance(payload, dict) or not isinstance(payload.get("id"), str):
        return None

    parent_id = text_at(payload, "parent_thread_id")
    if parent_id is None:
        parent_id = nested_parent_id(payload.get("source"))
    if parent_id is None:
        parent_id = nested_parent_id(payload.get("thread_source"))
    history_base = payload.get("history_base")
    own_start = payload.get("subagent_history_start_ordinal")
    return Rollout(
        path=path,
        id=payload["id"],
        parent_id=parent_id,
        forked_from_id=text_at(payload, "forked_from_id"),
        has_history_base=isinstance(history_base, dict),
        workdir=text_at(payload, "cwd") or "unknown",
        own_start=own_start
        if isinstance(own_start, int) and not isinstance(own_start, bool)
        else None,
    )


def nested_parent_id(value: Any) -> str | None:
    if isinstance(value, dict):
        if isinstance(value.get("parent_thread_id"), str):
            return value["parent_thread_id"]
        for nested in value.values():
            if id := nested_parent_id(nested):
                return id
    elif isinstance(value, list):
        for nested in value:
            if id := nested_parent_id(nested):
                return id
    return None


def parse_file(rollout: Rollout) -> Parsed:
    session = Session(
        id=rollout.id,
        root_id=rollout.id,
        is_main=rollout.parent_id is None,
        workdir=rollout.workdir,
        provider=Provider.CODEX,
    )
    model = None
    speed = None
    previous_total = None
    assistant = None
    pending_spawns: dict[str, int] = {}
    calls: list[AgentCall] = []
    records: list[Record] = []
    signatures: list[Signature] = []

    for line in jsonl(rollout.path):
        ordinal = line.get("ordinal")
        inherited = (
            rollout.own_start is not None
            and isinstance(ordinal, int)
            and not isinstance(ordinal, bool)
            and ordinal < rollout.own_start
        )
        payload = line.get("payload")
        if not isinstance(payload, dict):
            continue
        kind = line.get("type")
        payload_kind = payload.get("type")

        if kind == "turn_context":
            if isinstance(payload.get("model"), str):
                model = payload["model"]
            if isinstance(payload.get("cwd"), str):
                session.workdir = payload["cwd"]
            if isinstance(payload.get("service_tier"), str):
                speed = Speed.parse(payload["service_tier"])
        elif kind == "event_msg" and payload_kind == "thread_settings_applied":
            settings = payload.get("thread_settings")
            if isinstance(settings, dict) and isinstance(settings.get("service_tier"), str):
                speed = Speed.parse(settings["service_tier"])
        elif kind == "event_msg" and not inherited and payload_kind == "user_message":
            timestamp = parse_timestamp(line.get("timestamp"))
            if timestamp and has_text(payload.get("message")):
                records.append(Record(Event.activity(timestamp)))
            assistant = None
        elif kind == "event_msg" and not inherited and payload_kind == "task_complete":
            assistant = None
        elif kind == "event_msg" and payload_kind == "token_count":
            info = payload.get("info")
            if not isinstance(info, dict):
                continue
            total = raw_usage(info.get("total_token_usage"))
            if total is not None and total == previous_total:
                continue
            last = raw_usage(info.get("last_token_usage"))
            event_signature = signature(last) if last else None
            if event_signature:
                signatures.append(event_signature)
            raw = (
                last if last is not None else usage_delta(total, previous_total) if total else None
            )
            if total is not None:
                previous_total = total
            if inherited:
                continue

            event = None
            if raw is not None and (usage := normalize_usage(raw)).total > 0:
                event = Event(
                    timestamp=parse_timestamp(line.get("timestamp")),
                    usage=usage,
                    model=(info.get("model") if isinstance(info.get("model"), str) else model),
                    speed=speed,
                )
            if event is not None and event.timestamp is not None and assistant is not None:
                records[assistant].event = None
            assistant = None
            records.append(Record(event, event_signature))
        elif kind == "response_item" and not inherited:
            record_spawn(calls, pending_spawns, payload)
            timestamp = parse_timestamp(line.get("timestamp"))
            if has_response_activity(payload) and assistant is None and timestamp:
                records.append(Record(Event.activity(timestamp)))
                assistant = len(records) - 1

    return Parsed(session, calls, records, signatures)


def strip_replay(records: list[Record], parent: list[Signature]) -> list[Event]:
    output: list[Event] = []
    pending: list[Event] = []
    parent_pos = 0
    replaying = True
    for record in records:
        if not replaying:
            if record.event:
                output.append(record.event)
            continue
        if record.event:
            pending.append(record.event)
        if record.signature is None:
            continue
        while parent_pos < len(parent) and parent[parent_pos] != record.signature:
            parent_pos += 1
        if parent_pos < len(parent):
            parent_pos += 1
            pending.clear()
        else:
            replaying = False
            output.extend(pending)
            pending.clear()
    output.extend(pending)
    return output


def normalize_usage(raw: RawUsage) -> Usage:
    cache_read = min(raw.cached_input_tokens, raw.input_tokens)
    reasoning = min(raw.reasoning_output_tokens, raw.output_tokens)
    return Usage(
        input=raw.input_tokens - cache_read,
        cache_write_5m=raw.cache_write_input_tokens,
        cache_read=cache_read,
        output=raw.output_tokens - reasoning,
        reasoning=reasoning,
    )


def usage_delta(current: RawUsage, previous: RawUsage | None) -> RawUsage:
    previous = previous or RawUsage()
    return RawUsage(
        input_tokens=max(0, current.input_tokens - previous.input_tokens),
        cached_input_tokens=max(0, current.cached_input_tokens - previous.cached_input_tokens),
        cache_write_input_tokens=max(
            0, current.cache_write_input_tokens - previous.cache_write_input_tokens
        ),
        output_tokens=max(0, current.output_tokens - previous.output_tokens),
        reasoning_output_tokens=max(
            0, current.reasoning_output_tokens - previous.reasoning_output_tokens
        ),
        total_tokens=max(0, current.total_tokens - previous.total_tokens),
    )


def signature(usage: RawUsage) -> Signature:
    return (
        usage.input_tokens,
        usage.cached_input_tokens,
        usage.cache_write_input_tokens,
        usage.output_tokens,
        usage.reasoning_output_tokens,
        usage.total_tokens,
    )


def record_spawn(
    calls: list[AgentCall],
    pending: dict[str, int],
    payload: dict[str, Any],
) -> None:
    call_id = payload.get("call_id")
    id = payload.get("id") if call_id is None else call_id
    if not isinstance(id, str) or not id:
        return
    kind = payload.get("type")
    if kind in {"function_call", "custom_tool_call", "tool_search_call"}:
        name = payload.get("name")
        if (
            isinstance(name, str)
            and name.endswith("spawn_agent")
            and not any(x.tool_id == id for x in calls)
        ):
            pending[id] = len(calls)
            calls.append(AgentCall(id))
    elif kind in {"function_call_output", "custom_tool_call_output", "tool_search_output"}:
        if id in pending and "output" in payload:
            calls[pending.pop(id)].agent_id = find_agent_id(payload["output"])


def find_agent_id(value: Any) -> str | None:
    if isinstance(value, str):
        try:
            return find_agent_id(orjson.loads(value))
        except orjson.JSONDecodeError:
            return None
    if isinstance(value, dict):
        if isinstance(value.get("agent_id"), str):
            return value["agent_id"]
        for nested in value.values():
            if id := find_agent_id(nested):
                return id
    elif isinstance(value, list):
        for nested in value:
            if id := find_agent_id(nested):
                return id
    return None


def has_response_activity(payload: dict[str, Any]) -> bool:
    kind = payload.get("type")
    if kind == "message" and payload.get("role") == "assistant":
        content = payload.get("content")
        return isinstance(content, list) and any(
            isinstance(block, dict)
            and block.get("type") in {"output_text", "text"}
            and isinstance(block.get("text"), str)
            and bool(block["text"].strip())
            for block in content
        )
    return kind in {
        "function_call",
        "custom_tool_call",
        "tool_search_call",
        "web_search_call",
        "image_generation_call",
    }


def has_text(value: Any) -> bool:
    return isinstance(value, str) and bool(value.strip())


def raw_usage(value: Any) -> RawUsage | None:
    if not isinstance(value, dict):
        return None
    return RawUsage(
        input_tokens=integer(value.get("input_tokens")),
        cached_input_tokens=integer(value.get("cached_input_tokens")),
        cache_write_input_tokens=integer(value.get("cache_write_input_tokens")),
        output_tokens=integer(value.get("output_tokens")),
        reasoning_output_tokens=integer(value.get("reasoning_output_tokens")),
        total_tokens=integer(value.get("total_tokens")),
    )


def text_at(value: dict[str, Any], key: str) -> str | None:
    item = value.get(key)
    return item if isinstance(item, str) else None


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
