from dataclasses import dataclass, field
from datetime import datetime
from enum import StrEnum


class Provider(StrEnum):
    CLAUDE = "claude"
    CODEX = "codex"

    @property
    def pricing_key(self) -> str:
        return "anthropic" if self is Provider.CLAUDE else "openai"


class Speed(StrEnum):
    STANDARD = "standard"
    FAST = "fast"

    @classmethod
    def parse(cls, value: str) -> "Speed | None":
        if value in {"fast", "priority"}:
            return cls.FAST

        if value in {"standard", "default"}:
            return cls.STANDARD

        return None


@dataclass
class Usage:
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

    def __iadd__(self, other: "Usage") -> "Usage":
        return self + other


@dataclass
class Event:
    timestamp: datetime | None = None
    usage: Usage | None = None
    model: str | None = None
    speed: Speed | None = None
    identity: tuple[str, str | None] | None = None
    sidechain: bool = False
    failed: bool = False

    @classmethod
    def activity(cls, timestamp: datetime) -> "Event":
        return cls(timestamp=timestamp)


@dataclass
class Session:
    id: str
    root_id: str
    is_main: bool
    workdir: str
    provider: Provider
    events: list[Event] = field(default_factory=list)
