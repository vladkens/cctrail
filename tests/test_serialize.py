import json
from pathlib import Path

from cctrail import as_dict, as_json, iter_claude_sessions

FIXTURES = Path(__file__).parent / "fixtures" / "claude" / "projects"


def test_as_dict_includes_computed_properties_not_just_raw_fields():
    session = list(iter_claude_sessions(FIXTURES))[0]
    data = as_dict(session)

    # Raw stored field.
    assert data["id"] == "s-main-1"
    # Computed properties, not declared dataclass fields (including Usage's
    # own `total` property, one level down).
    assert data["usage"] == {
        "input": 15,
        "cache_write": 5,
        "cache_read": 300,
        "output": 33,
        "reasoning": 0,
        "total": 353,
    }
    assert data["tool_call_counts"] == {"Bash": 1, "Agent": 1}
    assert data["duration"] == 10.0  # timedelta -> seconds, not repr

    # Nested agent session serializes recursively, including its own properties.
    assert len(data["agents"]) == 1
    agent = data["agents"][0]
    assert agent["id"] == "a-explore-1"
    assert agent["duration"] == 5.0
    assert agent["tool_call_counts"] == {"Read": 1}


def test_as_dict_formats_timestamps_as_iso_strings():
    session = list(iter_claude_sessions(FIXTURES))[0]
    data = as_dict(session)
    assert data["messages"][0]["timestamp"] == "2026-01-01T00:00:00+00:00"


def test_as_json_round_trips_through_stdlib_json():
    session = list(iter_claude_sessions(FIXTURES))[0]
    parsed = json.loads(as_json(session))
    assert parsed["id"] == "s-main-1"
    assert parsed["usage"]["total"] == 15 + 5 + 300 + 33
