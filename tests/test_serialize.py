import json
from pathlib import Path

import cctrail
from cctrail import Message, Session, ToolCall, Usage, iter_claude_sessions
from cctrail.utils import json_dump, json_lines, json_load

FIXTURES = Path(__file__).parent / "fixtures" / "claude" / "projects"


def test_as_dict_includes_computed_properties_not_just_raw_fields():
    session = list(iter_claude_sessions(FIXTURES))[0]
    data = session.as_dict()

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
    data = session.as_dict()
    assert data["messages"][0]["timestamp"] == "2026-01-01T00:00:00+00:00"


def test_as_json_round_trips_through_stdlib_json():
    session = list(iter_claude_sessions(FIXTURES))[0]
    parsed = json.loads(session.as_json())
    assert parsed["id"] == "s-main-1"
    assert parsed["usage"]["total"] == 15 + 5 + 300 + 33


def test_all_models_have_json_methods():
    values = [
        Usage(input=1),
        ToolCall(id="call", name="Read", input={}),
        Message(role="user", timestamp=None),
        Session(id="session"),
    ]
    for value in values:
        assert isinstance(value.as_dict(), dict)
        assert json.loads(value.as_json()) == value.as_dict()


def test_json_functions_are_not_exported_from_package():
    assert not hasattr(cctrail, "as_dict")
    assert not hasattr(cctrail, "as_json")


def test_json_helpers_handle_invalid_values_and_jsonl(tmp_path: Path):
    assert json_load("invalid", "fallback") == "fallback"
    assert json_load("null", "fallback") is None
    assert json_load(json_dump({"text": "привет"})) == {"text": "привет"}

    path = tmp_path / "history.jsonl"
    path.write_bytes(b'invalid\n{"id":"one"}\n\n{"id":"two"}\n')
    assert list(json_lines(path)) == [{"id": "one"}, {"id": "two"}]
