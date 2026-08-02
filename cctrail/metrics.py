from collections import Counter
from collections.abc import Iterable
from dataclasses import dataclass, field
from datetime import date, datetime, timedelta, tzinfo

from .models import JsonTrait, Message, Session, Usage


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


def usage_rank(message: Message) -> int:
    assert message.usage is not None
    return message.usage.total


def usage_by_day(
    sessions: Iterable[Session], *, timezone: tzinfo | None = None
) -> dict[date, Usage]:
    """Sum token usage per calendar day, using the system timezone by default."""
    buckets: dict[date, Usage] = {}
    for _, message in usage_messages(sessions):
        if message.timestamp is None or message.usage is None:
            continue

        day = calendar_date(message.timestamp, timezone)
        buckets[day] = buckets.get(day, Usage()) + message.usage

    return buckets


def tool_call_counts(session: Session) -> Counter[str]:
    """Count tool calls in one session, excluding nested agent sessions."""
    return Counter(call.name for call in session.iter_tool_calls())


@dataclass
class ProjectSummary(JsonTrait):
    project: str
    usage: Usage = field(default_factory=Usage)
    session_count: int = 0
    first_activity: datetime | None = None
    last_activity: datetime | None = None


def by_project(sessions: Iterable[Session]) -> dict[str, ProjectSummary]:
    """Group sessions by working directory, summing usage across each session's agent tree."""
    sessions = list(sessions)
    result: dict[str, ProjectSummary] = {}
    for session in sessions:
        project = session.cwd or "unknown"
        summary = result.setdefault(project, ProjectSummary(project=project))
        if session.kind == "main":
            summary.session_count += 1

        for nested in session.walk():
            started, ended = nested.started_at, nested.ended_at
            if started and (summary.first_activity is None or started < summary.first_activity):
                summary.first_activity = started
            if ended and (summary.last_activity is None or ended > summary.last_activity):
                summary.last_activity = ended

    for session, message in usage_messages(sessions):
        project = session.cwd or "unknown"
        assert message.usage is not None
        result[project].usage = result[project].usage + message.usage

    return result


def calendar_date(timestamp: datetime, timezone: tzinfo | None) -> date:
    if timestamp.tzinfo is None:
        return timestamp.date()

    return timestamp.astimezone(timezone).date()


def active_time(events: Iterable[datetime], window: timedelta = timedelta(minutes=5)) -> timedelta:
    """Total wall-clock time covered by `events`, each active for `window` after it fires.

    Overlapping windows are merged, so idle gaps between bursts of activity
    (and between parallel sessions on the same project) aren't counted twice.
    """
    ordered = sorted(events)
    if not ordered:
        return timedelta()

    total = timedelta()
    start = end = ordered[0]
    for stamp in ordered[1:]:
        if stamp <= end + window:
            end = stamp
        else:
            total += end - start + window
            start = end = stamp

    total += end - start + window
    return total
