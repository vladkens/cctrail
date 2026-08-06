from datetime import UTC, date, datetime, timedelta, timezone
from pathlib import Path

from cctrail import (
    Message,
    Session,
    Usage,
    by_project,
    iter_claude_sessions,
    iter_sessions,
    usage_by_day,
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
    assert summary.first_activity is not None
    assert summary.last_activity is not None
    assert summary.as_dict()["session_count"] == 1


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
