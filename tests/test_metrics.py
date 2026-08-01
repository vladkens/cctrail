from datetime import date
from pathlib import Path

from cctrail import iter_claude_sessions
from cctrail.metrics import by_project, usage_by_day

FIXTURES = Path(__file__).parent / "fixtures" / "claude" / "projects"


def test_usage_by_day_includes_nested_agent_messages():
    sessions = list(iter_claude_sessions(FIXTURES))
    buckets = usage_by_day(sessions)

    # 2026-01-02 holds only a zero-usage isApiErrorMessage turn and must not
    # show up as a phantom all-zero day.
    assert list(buckets) == [date(2026, 1, 1)]
    day_total = buckets[date(2026, 1, 1)]

    # main session (135 + 57 + 161) + agent transcript (31 + 43)
    assert day_total.total == (135 + 57 + 161) + (31 + 43)


def test_by_project_groups_by_cwd_and_sums_agent_usage():
    sessions = list(iter_claude_sessions(FIXTURES))
    projects = by_project(sessions)

    assert list(projects) == ["/repo"]
    summary = projects["/repo"]
    assert summary.session_count == 1
    assert summary.usage.total == (135 + 57 + 161) + (31 + 43)
    assert summary.first_activity is not None
    assert summary.last_activity is not None
