from datetime import UTC, date, datetime, timedelta, timezone
from pathlib import Path

import cctrail.metrics as metrics
from cctrail import (
    Message,
    Session,
    Usage,
    by_project,
    iter_claude_sessions,
    iter_sessions,
    usage_by_day,
    usage_by_period,
)

FIXTURES = Path(__file__).parent / "fixtures" / "claude" / "projects"


def test_usage_by_day_includes_nested_agent_messages():
    sessions = list(iter_claude_sessions(FIXTURES))
    buckets = usage_by_day(sessions)

    # 2026-01-02 holds only a zero-usage isApiErrorMessage turn and must not
    # show up as a phantom all-zero day.
    assert list(buckets) == [date(2026, 1, 1)]
    day_total = buckets[date(2026, 1, 1)]

    # main session (135 + 57 + 161) + agent transcript (31 + 43)
    assert day_total.usage.total == (135 + 57 + 161) + (31 + 43)
    assert day_total.cost > 0


def test_by_project_groups_by_cwd_and_sums_agent_usage():
    sessions = list(iter_claude_sessions(FIXTURES))
    projects = by_project(sessions)

    assert list(projects) == ["/repo"]
    summary = projects["/repo"]
    assert summary.session_count == 1
    assert summary.usage.total == (135 + 57 + 161) + (31 + 43)
    assert summary.active_time == timedelta(minutes=5, seconds=10)
    assert summary.first_activity is not None
    assert summary.last_activity is not None
    assert summary.as_dict()["session_count"] == 1


def test_by_project_splits_overlapping_time_between_projects():
    start = datetime(2026, 1, 1, 10, 0, tzinfo=UTC)
    sessions = [
        Session(
            id="app",
            cwd="/app",
            messages=[Message("assistant", start, usage=Usage(input=1))],
        ),
        Session(
            id="app-worktree",
            cwd="/app-worktree",
            messages=[Message("assistant", start, usage=Usage(input=1))],
        ),
        Session(
            id="other",
            cwd="/other",
            messages=[
                Message(
                    "assistant",
                    start + timedelta(minutes=2),
                    usage=Usage(input=1),
                )
            ],
        ),
    ]

    projects = by_project(
        sessions,
        project_resolver=lambda project: "app" if project.startswith("/app") else project,
    )

    assert projects["app"].active_time == timedelta(minutes=3, seconds=30)
    assert projects["/other"].active_time == timedelta(minutes=3, seconds=30)
    assert sum((project.active_time for project in projects.values()), timedelta()) == timedelta(
        minutes=7
    )


def test_by_project_without_date_range_skips_calendar_conversion(monkeypatch):
    sessions = list(iter_claude_sessions(FIXTURES))

    def unexpected_conversion(*_args, **_kwargs):
        raise AssertionError("calendar conversion is unnecessary without a date range")

    monkeypatch.setattr(metrics, "calendar_date", unexpected_conversion)

    assert by_project(sessions)["/repo"].usage.total == (135 + 57 + 161) + (31 + 43)


def test_metrics_combine_claude_and_codex_sessions():
    fixtures = Path(__file__).parent / "fixtures"
    projects = by_project(iter_sessions(fixtures))

    summary = projects["/repo"]
    assert summary.session_count == 5
    assert summary.usage.total == (353 + 74) + 80 + (410 + 20 + 25)


def test_usage_identity_includes_request_id_except_for_sidechain_replays():
    timestamp = datetime(2026, 1, 1, tzinfo=UTC)
    session = Session(
        id="session",
        messages=[
            Message("assistant", timestamp, id="same", request_id="one", usage=Usage(input=10)),
            Message("assistant", timestamp, id="same", request_id="two", usage=Usage(input=20)),
            Message(
                "assistant",
                timestamp,
                id="same",
                request_id="replay",
                usage=Usage(input=1000),
                is_sidechain=True,
            ),
        ],
    )

    assert usage_by_day([session])[date(2026, 1, 1)].usage.total == 30


def test_usage_by_day_accepts_a_timezone_override():
    timestamp = datetime(2026, 1, 1, 23, 30, tzinfo=UTC)
    session = Session(
        id="session",
        messages=[Message("assistant", timestamp, usage=Usage(input=10))],
    )

    utc_plus_two = timezone(timedelta(hours=2))
    assert list(usage_by_day([session], timezone=utc_plus_two)) == [date(2026, 1, 2)]


def test_usage_cost_is_calculated_before_daily_aggregation():
    timestamp = datetime(2026, 1, 1, tzinfo=UTC)
    session = Session(
        id="session",
        provider="codex",
        messages=[
            Message(
                "assistant",
                timestamp,
                model="gpt-5.4",
                usage=Usage(input=200_000, output=1_000),
            ),
            Message(
                "assistant",
                timestamp,
                model="gpt-5.4",
                usage=Usage(input=200_000, output=1_000),
            ),
        ],
    )

    summary = usage_by_day([session])[date(2026, 1, 1)]
    assert summary.usage.input == 400_000
    assert summary.cost == 1.03


def test_usage_by_day_splits_active_time_at_midnight_and_merges_parallel_sessions():
    sessions = [
        Session(
            id="first",
            messages=[
                Message(
                    "assistant",
                    datetime(2026, 1, 1, 23, 58, tzinfo=UTC),
                    usage=Usage(input=1),
                ),
                Message(
                    "assistant",
                    datetime(2026, 1, 2, 0, 0, tzinfo=UTC),
                    usage=Usage(input=1),
                ),
            ],
        ),
        Session(
            id="second",
            messages=[
                Message(
                    "assistant",
                    datetime(2026, 1, 2, 0, 1, tzinfo=UTC),
                    usage=Usage(input=1),
                )
            ],
        ),
    ]

    buckets = usage_by_day(sessions, timezone=UTC)

    assert buckets[date(2026, 1, 1)].active_time == timedelta(minutes=2)
    assert buckets[date(2026, 1, 2)].active_time == timedelta(minutes=6)


def test_metrics_ignore_sessions_without_usage_and_previous_day_activity():
    previous = datetime(2026, 1, 1, 23, 58, tzinfo=UTC)
    current = datetime(2026, 1, 2, 0, 10, tzinfo=UTC)
    sessions = [
        Session(
            id="previous",
            cwd="/previous",
            messages=[Message("assistant", previous, usage=Usage(input=1))],
        ),
        Session(
            id="current",
            cwd="/current",
            messages=[Message("assistant", current, usage=Usage(input=1))],
        ),
        Session(
            id="failed",
            cwd="/failed",
            messages=[Message("user", current + timedelta(minutes=4))],
        ),
    ]

    projects = by_project(
        sessions,
        since=date(2026, 1, 2),
        until=date(2026, 1, 2),
        timezone=UTC,
    )
    buckets = usage_by_period(
        sessions,
        since=date(2026, 1, 2),
        until=date(2026, 1, 2),
        timezone=UTC,
    )

    assert list(projects) == ["/current"]
    assert projects["/current"].active_time == timedelta(minutes=5)
    assert buckets[date(2026, 1, 2)].active_time == timedelta(minutes=5)


def test_usage_by_period_groups_weeks_from_the_requested_first_weekday():
    session = Session(
        id="session",
        provider="codex",
        messages=[
            Message(
                "assistant",
                datetime(2026, 1, 4, tzinfo=UTC),
                model="gpt-5.4",
                usage=Usage(input=10),
            ),
            Message(
                "assistant",
                datetime(2026, 1, 5, tzinfo=UTC),
                model="gpt-5.4",
                usage=Usage(input=20),
            ),
        ],
    )

    sunday_first = usage_by_period([session], granularity="week", first_weekday=6)
    monday_first = usage_by_period([session], granularity="week", first_weekday=0)

    assert list(sunday_first) == [date(2026, 1, 4)]
    assert sunday_first[date(2026, 1, 4)].usage.input == 30
    assert list(monday_first) == [date(2025, 12, 29), date(2026, 1, 5)]


def test_usage_by_period_keeps_provider_and_fast_model_usage_separate():
    timestamp = datetime(2026, 1, 1, tzinfo=UTC)
    sessions = [
        Session(
            id="codex",
            provider="codex",
            messages=[
                Message(
                    "assistant",
                    timestamp,
                    model="gpt-5.4",
                    usage=Usage(input=10),
                ),
                Message(
                    "assistant",
                    timestamp,
                    model="gpt-5.4",
                    usage=Usage(input=20),
                    speed="fast",
                ),
            ],
        ),
        Session(
            id="claude",
            provider="claude",
            messages=[
                Message(
                    "assistant",
                    timestamp,
                    model="gpt-5.4",
                    usage=Usage(input=30),
                )
            ],
        ),
    ]

    models = usage_by_day(sessions)[date(2026, 1, 1)].models

    assert [(model.provider, model.model, model.speed, model.usage.input) for model in models] == [
        ("claude", "gpt-5.4", None, 30),
        ("codex", "gpt-5.4", "fast", 20),
        ("codex", "gpt-5.4", None, 10),
    ]


def test_by_project_filters_every_metric_to_the_date_range():
    session = Session(
        id="session",
        cwd="/repo",
        messages=[
            Message(
                "assistant",
                datetime(2026, 1, 1, tzinfo=UTC),
                usage=Usage(input=10),
            ),
            Message(
                "assistant",
                datetime(2026, 1, 2, tzinfo=UTC),
                usage=Usage(input=20),
            ),
        ],
    )

    summary = by_project(
        [session],
        since=date(2026, 1, 2),
        until=date(2026, 1, 2),
        timezone=UTC,
    )["/repo"]

    assert summary.usage.input == 20
    assert summary.session_count == 1
    assert summary.first_activity == datetime(2026, 1, 2, tzinfo=UTC)
    assert summary.last_activity == datetime(2026, 1, 2, tzinfo=UTC)
    assert summary.active_time == timedelta(minutes=5)
