from collections import Counter
from collections.abc import Iterator
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from typing import Any, Literal


@dataclass
class Usage:
    input: int = 0
    cache_write: int = 0
    cache_read: int = 0
    output: int = 0

    @property
    def total(self) -> int:
        return self.input + self.cache_write + self.cache_read + self.output

    def __add__(self, other: "Usage") -> "Usage":
        return Usage(
            input=self.input + other.input,
            cache_write=self.cache_write + other.cache_write,
            cache_read=self.cache_read + other.cache_read,
            output=self.output + other.output,
        )


@dataclass
class ToolCall:
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
class Message:
    role: Literal["user", "assistant"]
    timestamp: datetime | None
    id: str | None = None
    request_id: str | None = None
    text: str = ""
    model: str | None = None
    usage: Usage | None = None
    tool_calls: list[ToolCall] = field(default_factory=list)
    turn_duration: timedelta | None = None
    is_api_error: bool = False
    is_sidechain: bool = False


@dataclass
class Session:
    id: str
    kind: Literal["main", "agent"] = "main"
    provider: str = "claude"
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

    @property
    def tool_calls(self) -> list[ToolCall]:
        return [call for message in self.messages for call in message.tool_calls]

    @property
    def tool_call_counts(self) -> Counter[str]:
        return Counter(call.name for call in self.tool_calls)

    @property
    def agents(self) -> list["Session"]:
        return [call.agent for call in self.tool_calls if call.agent is not None]

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
        for agent in self.agents:
            yield from agent.walk()
