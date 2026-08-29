from collections import defaultdict
from collections.abc import Iterable
from dataclasses import dataclass, field
from datetime import UTC, date, datetime, time, timedelta
from enum import StrEnum

from . import pricing
from .config import Config, StatsBy
from .model import Event, Provider, Session, Speed, Usage

ACTIVITY_WINDOW = timedelta(minutes=5)
MICROS_PER_SECOND = 1_000_000
RootKey = tuple[Provider, str]


class Period(StrEnum):
    TODAY = "today"
    YESTERDAY = "yesterday"
    THIS_WEEK = "this-week"
    LAST_WEEK = "last-week"
    THIS_MONTH = "this-month"
    LAST_MONTH = "last-month"
    ALL = "all"


class Granularity(StrEnum):
    DAY = "day"
    WEEK = "week"
    MONTH = "month"

    def bucket(self, value: date) -> date:
        if self is Granularity.DAY:
            return value

        if self is Granularity.WEEK:
            return value - timedelta(days=value.weekday())

        return value.replace(day=1)

    def bounds(self, start: date, query: "DateRange") -> tuple[date, date]:
        end = self.bucket_end(start)
        return (
            max(start, query.since) if query.since else start,
            min(end, query.until) if query.until else end,
        )

    def label(self, start: date, query: "DateRange") -> str:
        if self is Granularity.DAY:
            return str(start)
        first = max(start, query.since) if query.since else start
        end = self.bucket_end(start)
        last = min(end, query.until) if query.until else end
        if self is Granularity.MONTH and first == start and last == end:
            return start.strftime("%Y-%m")
        return str(first) if first == last else f"{first} – {last:%m-%d}"

    def bucket_end(self, start: date) -> date:
        if self is Granularity.DAY:
            return start
        if self is Granularity.WEEK:
            return start + timedelta(days=6)
        return next_month(start) - timedelta(days=1)


@dataclass(frozen=True)
class DateRange:
    since: date | None = None
    until: date | None = None

    @classmethod
    def resolve(
        cls,
        period: Period | None = None,
        since: date | None = None,
        until: date | None = None,
    ) -> "DateRange":
        if since and until and since > until:
            raise ValueError("--since must not be later than --until")
        if period is None:
            return cls(since, until)

        today = datetime.now().astimezone().date()
        day = timedelta(days=1)
        if period is Period.ALL:
            return cls()
        if period is Period.TODAY:
            return cls(today, today)
        if period is Period.YESTERDAY:
            return cls(today - day, today - day)
        if period is Period.THIS_WEEK:
            start = Granularity.WEEK.bucket(today)
            return cls(start, start + timedelta(days=6))
        if period is Period.LAST_WEEK:
            end = Granularity.WEEK.bucket(today) - day
            return cls(end - timedelta(days=6), end)
        if period is Period.THIS_MONTH:
            start = month_start(today)
            return cls(start, next_month(start) - day)
        end = month_start(today) - day
        return cls(month_start(end), end)

    def contains(self, timestamp: datetime) -> bool:
        return self.contains_day(local_date(timestamp))

    def contains_day(self, day: date) -> bool:
        return (self.since is None or day >= self.since) and (
            self.until is None or day <= self.until
        )


@dataclass
class ModelSummary:
    provider: Provider
    model: str
    fast: bool
    usage: Usage = field(default_factory=Usage)
    cost: float = 0.0


@dataclass
class UsageSummary:
    usage: Usage = field(default_factory=Usage)
    cost: float = 0.0
    active_time: timedelta = field(default_factory=timedelta)
    models: list[ModelSummary] = field(default_factory=list)


@dataclass
class Stat:
    name: str = ""
    sessions: int = 0
    usage: Usage = field(default_factory=Usage)
    cost: float = 0.0
    active_time: timedelta = field(default_factory=timedelta)
    first_activity: datetime | None = None
    last_activity: datetime | None = None


def usage_by_period(
    sessions: list[Session],
    granularity: Granularity,
    date_range: DateRange,
) -> dict[date, UsageSummary]:
    summaries: dict[date, UsageSummary] = {}
    models: defaultdict[date, dict[tuple[Provider, str, bool], ModelSummary]] = defaultdict(dict)
    billable_buckets: set[tuple[RootKey, date]] = set()

    for session, event in billable_events(sessions):
        timestamp = event.timestamp
        if timestamp is None or not date_range.contains(timestamp):
            continue
        bucket = granularity.bucket(local_date(timestamp))
        usage = event.usage
        assert usage is not None
        event_cost = pricing.cost(session.provider, event.model, usage, event.speed)
        summary = summaries.setdefault(bucket, UsageSummary())
        summary.usage += usage
        summary.cost += event_cost
        billable_buckets.add((root(session), bucket))

        fast = event.speed is Speed.FAST
        model = event.model or "unknown"
        key = session.provider, model, fast
        model_summary = models[bucket].setdefault(key, ModelSummary(session.provider, model, fast))
        model_summary.usage += usage
        model_summary.cost += event_cost

    stamps = [
        event.timestamp
        for session in sessions
        for event in session.events
        if not event.failed
        and event.timestamp is not None
        and date_range.contains(event.timestamp)
        and (
            root(session),
            granularity.bucket(local_date(event.timestamp)),
        )
        in billable_buckets
    ]
    for day, duration in active_by_day(stamps).items():
        bucket = granularity.bucket(day)
        if date_range.contains_day(day) and bucket in summaries:
            summaries[bucket].active_time += duration

    for bucket, values in models.items():
        summaries[bucket].models = sorted(
            values.values(),
            key=lambda x: (-x.usage.total, x.provider, x.model, x.fast),
        )
    return dict(sorted(summaries.items()))


def stats_by(
    sessions: list[Session],
    date_range: DateRange,
    config: Config,
    by: StatsBy,
) -> list[Stat]:
    events = billable_events(sessions)
    bounded = date_range.since is not None or date_range.until is not None
    billable_roots = {
        root(session)
        for session, event in events
        if not bounded or event.timestamp is not None and date_range.contains(event.timestamp)
    }
    main_roots = {root(x) for x in sessions if x.is_main}
    stats: dict[str, Stat] = {}
    sessions_by_name: defaultdict[str, set[RootKey]] = defaultdict(set)
    stamps_by_name: defaultdict[str, list[datetime]] = defaultdict(list)

    for session in sessions:
        session_root = root(session)
        if session_root not in billable_roots:
            continue
        stamps = [
            x.timestamp
            for x in session.events
            if not x.failed
            and x.timestamp is not None
            and (not bounded or date_range.contains(x.timestamp))
        ]
        if not stamps:
            continue
        name = config.map_workdir(session.workdir, by)
        stamps_by_name[name].extend(stamps)
        stat = stats.setdefault(name, Stat(name))
        first, last = min(stamps), max(stamps)
        stat.first_activity = min(stat.first_activity, first) if stat.first_activity else first
        stat.last_activity = max(stat.last_activity, last) if stat.last_activity else last
        if session_root in main_roots:
            sessions_by_name[name].add(session_root)

    for session, event in events:
        if bounded and (event.timestamp is None or not date_range.contains(event.timestamp)):
            continue
        usage = event.usage
        assert usage is not None
        name = config.map_workdir(session.workdir, by)
        stat = stats.setdefault(name, Stat(name))
        stat.usage += usage
        stat.cost += pricing.cost(session.provider, event.model, usage, event.speed)

    for name, duration in active_by_name(stamps_by_name, date_range).items():
        if name in stats:
            stats[name].active_time = duration
    for name, roots in sessions_by_name.items():
        if name in stats:
            stats[name].sessions = len(roots)
    return sorted(stats.values(), key=lambda x: (-x.active_time, -x.usage.total, x.name))


def combine_stats(stats: Iterable[Stat]) -> Stat:
    total = Stat("total")
    for stat in stats:
        total.sessions += stat.sessions
        total.usage += stat.usage
        total.cost += stat.cost
        total.active_time += stat.active_time
        if stat.first_activity is not None:
            total.first_activity = (
                min(total.first_activity, stat.first_activity)
                if total.first_activity
                else stat.first_activity
            )
        if stat.last_activity is not None:
            total.last_activity = (
                max(total.last_activity, stat.last_activity)
                if total.last_activity
                else stat.last_activity
            )
    return total


def billable_events(sessions: list[Session]) -> list[tuple[Session, Event]]:
    Candidate = tuple[int, int]
    anonymous: list[Candidate] = []
    direct: dict[tuple[Provider, str, str | None], Candidate] = {}
    sidechains: dict[tuple[Provider, str], Candidate] = {}

    for si, session in enumerate(sessions):
        for ei, event in enumerate(session.events):
            if event.failed or event.usage is None or event.usage.total == 0:
                continue
            candidate = si, ei
            if event.identity is None:
                anonymous.append(candidate)
                continue
            id, request = event.identity
            if event.sidechain:
                keep_best(sidechains, (session.provider, id), candidate, sessions)
            else:
                keep_best(direct, (session.provider, id, request), candidate, sessions)

    direct_ids = {(provider, id) for provider, id, _ in direct}
    candidates = (
        anonymous
        + list(direct.values())
        + [x for key, x in sidechains.items() if key not in direct_ids]
    )
    return [(sessions[si], sessions[si].events[ei]) for si, ei in sorted(candidates)]


def keep_best(
    candidates: dict,
    key: tuple,
    candidate: tuple[int, int],
    sessions: list[Session],
) -> None:
    def rank(value: tuple[int, int]) -> tuple[int, bool]:
        event = sessions[value[0]].events[value[1]]
        return event.usage.total if event.usage else 0, event.speed is not None

    if key not in candidates or rank(candidate) > rank(candidates[key]):
        candidates[key] = candidate


def root(session: Session) -> RootKey:
    return session.provider, session.root_id


def local_date(timestamp: datetime) -> date:
    return timestamp.astimezone().date()


def month_start(value: date) -> date:
    return value.replace(day=1)


def next_month(value: date) -> date:
    year, month = (value.year + 1, 1) if value.month == 12 else (value.year, value.month + 1)
    return date(year, month, 1)


def activity_windows(stamps: Iterable[datetime]) -> list[tuple[datetime, datetime]]:
    ordered = sorted(set(stamps))
    if not ordered:
        return []
    start = ordered[0]
    end = start + ACTIVITY_WINDOW
    result = []
    for stamp in ordered[1:]:
        if stamp <= end:
            end = max(end, stamp + ACTIVITY_WINDOW)
        else:
            result.append((start, end))
            start, end = stamp, stamp + ACTIVITY_WINDOW
    result.append((start, end))
    return result


def active_by_day(stamps: Iterable[datetime]) -> dict[date, timedelta]:
    result: defaultdict[date, timedelta] = defaultdict(timedelta)
    for start, end in activity_windows(stamps):
        for day, duration in split_by_day(start, end):
            result[day] += duration
    return dict(sorted(result.items()))


def active_by_name(
    stamps_by_name: dict[str, list[datetime]],
    date_range: DateRange,
) -> dict[str, timedelta]:
    changes: defaultdict[datetime, list[tuple[str, bool]]] = defaultdict(list)
    for name, stamps in stamps_by_name.items():
        for start, end in activity_windows(stamps):
            changes[start].append((name, True))
            changes[end].append((name, False))

    bounded = date_range.since is not None or date_range.until is not None
    active: set[str] = set()
    durations: defaultdict[str, int] = defaultdict(int)
    previous = None
    for timestamp in sorted(changes):
        if previous is not None and active:
            if bounded:
                for day, duration in split_by_day(previous, timestamp):
                    if date_range.contains_day(day):
                        distribute(durations, active, micros(duration))
            else:
                distribute(durations, active, micros(timestamp - previous))
        for name, begins in changes[timestamp]:
            if begins:
                active.add(name)
            else:
                active.discard(name)
        previous = timestamp
    return {name: timedelta(seconds=value) for name, value in whole_seconds(durations).items()}


def split_by_day(start: datetime, end: datetime) -> list[tuple[date, timedelta]]:
    result = []
    cursor = start
    while cursor < end:
        local = cursor.astimezone()
        next_day = local.date() + timedelta(days=1)
        boundary = datetime.combine(next_day, time.min).astimezone(UTC)
        boundary = min(boundary, end)
        result.append((local.date(), boundary - cursor))
        cursor = boundary
    return result


def micros(duration: timedelta) -> int:
    return duration // timedelta(microseconds=1)


def distribute(result: dict[str, int], names: set[str], duration: int) -> None:
    share, remainder = divmod(duration, len(names))
    for i, name in enumerate(sorted(names)):
        result[name] = result.get(name, 0) + share + (i < remainder)


def whole_seconds(durations: dict[str, int]) -> dict[str, int]:
    total = sum(durations.values()) // MICROS_PER_SECOND
    seconds = {name: value // MICROS_PER_SECOND for name, value in durations.items()}
    remainder = total - sum(seconds.values())
    fractions = sorted(
        durations.items(),
        key=lambda x: (-(x[1] % MICROS_PER_SECOND), x[0]),
    )
    for name, _ in fractions[:remainder]:
        seconds[name] += 1
    return seconds
