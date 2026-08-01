from collections.abc import Iterable
from dataclasses import dataclass, field
from datetime import date, datetime, timedelta

from .models import Session, Usage


def usage_by_day(sessions: Iterable[Session]) -> dict[date, Usage]:
    """Sum token usage per calendar day, across each session and its nested agents."""
    buckets: dict[date, Usage] = {}
    for session in sessions:
        for s in session.walk():
            for message in s.messages:
                if message.timestamp is None or message.usage is None or message.is_api_error:
                    continue
                day = message.timestamp.date()
                buckets[day] = buckets.get(day, Usage()) + message.usage
    return buckets


@dataclass
class ProjectSummary:
    project: str
    usage: Usage = field(default_factory=Usage)
    session_count: int = 0
    first_activity: datetime | None = None
    last_activity: datetime | None = None


def by_project(sessions: Iterable[Session]) -> dict[str, ProjectSummary]:
    """Group sessions by working directory, summing usage across each session's agent tree."""
    result: dict[str, ProjectSummary] = {}
    for session in sessions:
        project = session.cwd or "unknown"
        summary = result.setdefault(project, ProjectSummary(project=project))
        summary.session_count += 1
        for s in session.walk():
            summary.usage = summary.usage + s.usage
        started, ended = session.started_at, session.ended_at
        if started and (summary.first_activity is None or started < summary.first_activity):
            summary.first_activity = started
        if ended and (summary.last_activity is None or ended > summary.last_activity):
            summary.last_activity = ended
    return result


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
