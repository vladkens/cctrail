from collections import Counter, defaultdict
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field
from datetime import date, datetime, timedelta, tzinfo
from typing import Literal

from .models import JsonTrait, Message, Session, Usage
from .pricing import calculate_cost


def usage_messages(sessions: Iterable[Session]) -> list[tuple[Session, Message]]:
    """Return billable messages using Claude's response and sidechain identity rules."""
    anonymous: list[tuple[Session, Message]] = []
    direct: dict[tuple[str, str, str | None], tuple[Session, Message]] = {}
    sidechains: dict[tuple[str, str], tuple[Session, Message]] = {}
    for session in sessions:
        for nested in session.walk():
            for message in nested.messages:
                if message.usage is None or message.usage.total == 0 or message.is_api_error:
                    continue

                if message.id is None:
                    anonymous.append((session, message))
                    continue

                message_key = session.provider, message.id
                if message.is_sidechain:
                    existing = sidechains.get(message_key)
                    if existing is None or usage_rank(message) > usage_rank(existing[1]):
                        sidechains[message_key] = session, message
                else:
                    key = *message_key, message.request_id
                    existing = direct.get(key)
                    if existing is None or usage_rank(message) > usage_rank(existing[1]):
                        direct[key] = session, message

    direct_message_ids = {(provider, message_id) for provider, message_id, _ in direct}
    return (
        anonymous
        + list(direct.values())
        + [candidate for key, candidate in sidechains.items() if key not in direct_message_ids]
    )


def usage_rank(message: Message) -> tuple[int, bool]:
    assert message.usage is not None
    return message.usage.total, message.speed is not None


@dataclass
class ModelSummary(JsonTrait):
    provider: str
    model: str
    speed: Literal["fast"] | None = None
    usage: Usage = field(default_factory=Usage)
    cost: float = 0.0


@dataclass
class UsageSummary(JsonTrait):
    usage: Usage = field(default_factory=Usage)
    cost: float = 0.0
    active_time: timedelta = field(default_factory=timedelta)
    models: list[ModelSummary] = field(default_factory=list)


def usage_by_day(
    sessions: Iterable[Session], *, timezone: tzinfo | None = None
) -> dict[date, UsageSummary]:
    """Summarize usage and estimated activity per calendar day."""
    return usage_by_period(sessions, timezone=timezone)


def usage_by_period(
    sessions: Iterable[Session],
    *,
    granularity: Literal["day", "week", "month"] = "day",
    since: date | None = None,
    until: date | None = None,
    timezone: tzinfo | None = None,
    first_weekday: int = 0,
) -> dict[date, UsageSummary]:
    """Summarize usage by calendar day, week, or month."""
    sessions = list(sessions)
    buckets: dict[date, UsageSummary] = {}
    models: defaultdict[date, dict[tuple[str, str, str | None], ModelSummary]] = defaultdict(dict)
    for session, message in usage_messages(sessions):
        if message.timestamp is None or message.usage is None:
            continue

        day = calendar_date(message.timestamp, timezone)
        if not date_in_range(day, since, until):
            continue

        bucket = period_start(day, granularity, first_weekday)
        summary = buckets.setdefault(bucket, UsageSummary())
        summary.usage = summary.usage + message.usage
        cost = calculate_cost(
            message.model or "",
            message.usage,
            provider=session.provider,
            speed=message.speed,
        )
        summary.cost += cost

        speed = "fast" if message.speed == "fast" else None
        model_key = session.provider, message.model or "unknown", speed
        model = models[bucket].setdefault(
            model_key,
            ModelSummary(
                provider=session.provider,
                model=message.model or "unknown",
                speed=speed,
            ),
        )
        model.usage = model.usage + message.usage
        model.cost += cost

    events = activity_stamps(sessions)
    for day, duration in active_time_by_day(events, timezone=timezone).items():
        if not date_in_range(day, since, until):
            continue

        bucket = period_start(day, granularity, first_weekday)
        buckets.setdefault(bucket, UsageSummary()).active_time += duration

    for bucket, summaries in models.items():
        buckets[bucket].models = sorted(
            summaries.values(),
            key=lambda summary: (
                -summary.usage.total,
                summary.provider,
                summary.model,
                summary.speed or "",
            ),
        )

    return buckets


def tool_call_counts(session: Session) -> Counter[str]:
    """Count tool calls in one session, excluding nested agent sessions."""
    return Counter(call.name for call in session.iter_tool_calls())


@dataclass
class ProjectSummary(JsonTrait):
    project: str
    usage: Usage = field(default_factory=Usage)
    cost: float = 0.0
    active_time: timedelta = field(default_factory=timedelta)
    session_count: int = 0
    first_activity: datetime | None = None
    last_activity: datetime | None = None


def by_project(
    sessions: Iterable[Session],
    *,
    since: date | None = None,
    until: date | None = None,
    timezone: tzinfo | None = None,
    project_resolver: Callable[[str], str] | None = None,
) -> dict[str, ProjectSummary]:
    """Group sessions by working directory, summing usage across each session's agent tree."""
    sessions = list(sessions)
    result: dict[str, ProjectSummary] = {}
    events: defaultdict[str, list[datetime]] = defaultdict(list)
    for session in sessions:
        project = session.cwd or "unknown"
        if project_resolver is not None:
            project = project_resolver(project)
        all_stamps = [stamp for nested in session.walk() for stamp in nested.activity_stamps()]
        events[project].extend(all_stamps)
        stamps = [
            stamp
            for stamp in all_stamps
            if date_in_range(calendar_date(stamp, timezone), since, until)
        ]
        if stamps:
            summary = result.setdefault(project, ProjectSummary(project=project))
            if session.kind == "main":
                summary.session_count += 1

            started, ended = min(stamps), max(stamps)
            if summary.first_activity is None or started < summary.first_activity:
                summary.first_activity = started
            if summary.last_activity is None or ended > summary.last_activity:
                summary.last_activity = ended

    for session, message in usage_messages(sessions):
        if message.timestamp is not None:
            day = calendar_date(message.timestamp, timezone)
            if not date_in_range(day, since, until):
                continue
        elif since is not None or until is not None:
            continue

        project = session.cwd or "unknown"
        if project_resolver is not None:
            project = project_resolver(project)
        assert message.usage is not None
        summary = result.setdefault(project, ProjectSummary(project=project))
        summary.usage = summary.usage + message.usage
        summary.cost += calculate_cost(
            message.model or "",
            message.usage,
            provider=session.provider,
            speed=message.speed,
        )

    for project, project_events in events.items():
        duration = sum(
            (
                duration
                for day, duration in active_time_by_day(
                    project_events,
                    timezone=timezone,
                ).items()
                if date_in_range(day, since, until)
            ),
            timedelta(),
        )
        if duration:
            result.setdefault(project, ProjectSummary(project=project)).active_time = duration

    return result


def date_in_range(value: date, since: date | None, until: date | None) -> bool:
    return (since is None or value >= since) and (until is None or value <= until)


def period_start(
    value: date,
    granularity: Literal["day", "week", "month"],
    first_weekday: int = 0,
) -> date:
    if granularity == "day":
        return value
    if granularity == "week":
        return value - timedelta(days=(value.weekday() - first_weekday) % 7)
    if granularity == "month":
        return value.replace(day=1)
    raise ValueError(f"Unsupported granularity: {granularity}")


def calendar_date(timestamp: datetime, timezone: tzinfo | None) -> date:
    if timestamp.tzinfo is None:
        return timestamp.date()

    return timestamp.astimezone(timezone).date()


def activity_stamps(sessions: Iterable[Session]) -> list[datetime]:
    return [
        stamp
        for session in sessions
        for nested in session.walk()
        for stamp in nested.activity_stamps()
    ]


def activity_intervals(
    events: Iterable[datetime], window: timedelta = timedelta(minutes=5)
) -> list[tuple[datetime, datetime]]:
    ordered = sorted(set(events))
    if not ordered:
        return []

    start = ordered[0]
    end = start + window
    intervals: list[tuple[datetime, datetime]] = []
    for stamp in ordered[1:]:
        interval_end = stamp + window
        if stamp <= end:
            end = max(end, interval_end)
        else:
            intervals.append((start, end))
            start, end = stamp, interval_end
    intervals.append((start, end))
    return intervals


def active_time(events: Iterable[datetime], window: timedelta = timedelta(minutes=5)) -> timedelta:
    """Total wall-clock time covered by `events`, each active for `window` after it fires.

    Overlapping windows are merged, so idle gaps between bursts of activity
    (and between parallel sessions on the same project) aren't counted twice.
    """
    return sum((end - start for start, end in activity_intervals(events, window)), timedelta())


def active_time_by_day(
    events: Iterable[datetime],
    *,
    timezone: tzinfo | None = None,
    window: timedelta = timedelta(minutes=5),
) -> dict[date, timedelta]:
    result: defaultdict[date, timedelta] = defaultdict(timedelta)
    for start, end in activity_intervals(events, window):
        cursor = start
        while cursor < end:
            local = cursor if cursor.tzinfo is None else cursor.astimezone(timezone)
            boundary = (local + timedelta(days=1)).replace(
                hour=0,
                minute=0,
                second=0,
                microsecond=0,
            )
            if cursor.tzinfo is not None:
                boundary = boundary.astimezone(cursor.tzinfo)
            part_end = min(end, boundary)
            result[local.date()] += part_end - cursor
            cursor = part_end

    return dict(result)
