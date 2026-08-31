from datetime import UTC, date, datetime, timedelta

from cctrail.config import Config, StatsBy
from cctrail.metrics import DateRange, Granularity, activity_windows, stats_by
from cctrail.model import Event, Provider, Session, Speed, Usage


def session(id: str, timestamp: datetime) -> Session:
    return Session(
        id=id,
        root_id=id,
        is_main=True,
        workdir=id,
        provider=Provider.CODEX,
        events=[
            Event(
                timestamp=timestamp,
                usage=Usage(input=1),
                speed=Speed.STANDARD,
            )
        ],
    )


def test_divides_parallel_time_between_workdirs():
    timestamp = datetime(2026, 1, 1, 10, tzinfo=UTC)
    stats = stats_by(
        [session("/first", timestamp), session("/second", timestamp)],
        DateRange(),
        Config(),
        StatsBy.WORKDIR,
    )

    assert len(stats) == 2
    assert all(x.active_time == timedelta(seconds=150) for x in stats)


def test_weeks_start_on_monday():
    assert Granularity.WEEK.bucket(date(2026, 8, 5)) == date(2026, 8, 3)


def test_clips_calendar_bucket_to_query():
    query = DateRange(date(2026, 8, 10), date(2026, 8, 20))

    assert Granularity.MONTH.bounds(date(2026, 8, 1), query) == (
        date(2026, 8, 10),
        date(2026, 8, 20),
    )


def test_merges_overlapping_activity_windows():
    start = datetime(2026, 1, 1, 10, tzinfo=UTC)

    assert activity_windows([start, start + timedelta(minutes=2), start]) == [
        (start, start + timedelta(minutes=7))
    ]


def test_failed_events_are_not_billed_or_counted_as_activity():
    start = datetime(2026, 1, 1, 10, tzinfo=UTC)
    value = session("/repo", start)
    value.events.append(
        Event(
            timestamp=start + timedelta(hours=1),
            usage=Usage(input=100),
            failed=True,
        )
    )

    stat = stats_by([value], DateRange(), Config(), StatsBy.WORKDIR)[0]

    assert stat.usage.total == 1
    assert stat.active_time == timedelta(minutes=5)
    assert stat.last_activity == start
