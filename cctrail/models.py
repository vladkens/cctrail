from collections.abc import Iterator
from dataclasses import asdict, dataclass, field
from datetime import datetime, timedelta
from typing import Any, Literal

from .utils import json_dump


@dataclass
class JsonTrait:
    def as_dict(self) -> dict[str, Any]:
        """Return the model's stored dataclass fields as a dictionary.

        Every declared field is included recursively. For a Session this means
        that messages, their tool calls, and linked agents are serialized.
        Computed properties such as `usage`, `started_at`, `ended_at`, and
        `duration`, along with values exposed through methods, are not included.
        """
        return asdict(self)

    def as_json(self, indent: int | None = None) -> str:
        return json_dump(self.as_dict(), default=str, indent=indent)


@dataclass
class Usage(JsonTrait):
    input: int = 0
    cache_write_5m: int = 0
    cache_write_1h: int = 0
    cache_read: int = 0
    output: int = 0
    reasoning: int = 0

    @property
    def cache_write(self) -> int:
        return self.cache_write_5m + self.cache_write_1h

    @property
    def total(self) -> int:
        return self.input + self.cache_write + self.cache_read + self.output + self.reasoning

    def __add__(self, other: "Usage") -> "Usage":
        return Usage(
            input=self.input + other.input,
            cache_write_5m=self.cache_write_5m + other.cache_write_5m,
            cache_write_1h=self.cache_write_1h + other.cache_write_1h,
            cache_read=self.cache_read + other.cache_read,
            output=self.output + other.output,
            reasoning=self.reasoning + other.reasoning,
        )


@dataclass
class ToolCall(JsonTrait):
    id: str
    name: str
    input: dict[str, Any]
    started_at: datetime | None = None
    ended_at: datetime | None = None
    output_text: str | None = None
    is_error: bool = False
    raw_result: Any = None
    agent: "Session | None" = None

    @property
    def duration(self) -> timedelta | None:
        if self.started_at is None or self.ended_at is None:
            return None
        return self.ended_at - self.started_at


@dataclass
class Message(JsonTrait):
    role: Literal["user", "assistant"]
    timestamp: datetime | None
    id: str | None = None
    request_id: str | None = None
    text: str = ""
    model: str | None = None
    usage: Usage | None = None
    speed: Literal["standard", "fast"] | None = None
    tool_calls: list[ToolCall] = field(default_factory=list)
    turn_duration: timedelta | None = None
    is_api_error: bool = False
    is_sidechain: bool = False


@dataclass
class Session(JsonTrait):
    id: str
    kind: Literal["main", "agent"] = "main"
    provider: Literal["claude", "codex"] = "claude"
    agent_type: str | None = None
    title: str | None = None
    cwd: str | None = None
    git_branch: str | None = None
    messages: list[Message] = field(default_factory=list)
    duration_override: timedelta | None = None

    @property
    def usage(self) -> Usage:
        result = Usage()
        for message in self.messages:
            if message.usage:
                result = result + message.usage

        return result

    def iter_tool_calls(self) -> Iterator[ToolCall]:
        for message in self.messages:
            yield from message.tool_calls

    def iter_agents(self) -> Iterator["Session"]:
        for call in self.iter_tool_calls():
            if call.agent is not None:
                yield call.agent

    def activity_stamps(self) -> list[datetime]:
        # A failed API call carries no real work, and including it here would
        # let one late retry blow up the session's apparent span.
        return [
            m.timestamp for m in self.messages if m.timestamp is not None and not m.is_api_error
        ]

    @property
    def started_at(self) -> datetime | None:
        stamps = self.activity_stamps()
        return min(stamps) if stamps else None

    @property
    def ended_at(self) -> datetime | None:
        stamps = self.activity_stamps()
        return max(stamps) if stamps else None

    @property
    def duration(self) -> timedelta | None:
        if self.duration_override is not None:
            return self.duration_override

        started, ended = self.started_at, self.ended_at
        return ended - started if started and ended else None

    def walk(self) -> Iterator["Session"]:
        """Yield this session and every nested agent run, depth-first."""
        yield self
        for agent in self.iter_agents():
            yield from agent.walk()
