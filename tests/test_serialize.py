import json
from datetime import datetime
from pathlib import Path

import cctrail
from cctrail import Message, ProjectSummary, Session, ToolCall, Usage, iter_claude_sessions
from cctrail.utils import json_dump, json_lines, json_load

FIXTURES = Path(__file__).parent / "fixtures" / "claude" / "projects"


def test_as_dict_serializes_stored_model_fields():
    session = list(iter_claude_sessions(FIXTURES))[0]
    data = session.as_dict()

    assert data["id"] == "s-main-1"
    assert data["messages"][1]["usage"] == {
        "input": 10,
        "cache_write": 5,
        "cache_read": 100,
        "output": 20,
        "reasoning": 0,
    }
    assert "usage" not in data
    assert "duration" not in data
    assert "agents" not in data
    assert "tool_calls" not in data

    agent = next(
        call["agent"]
        for message in data["messages"]
        for call in message["tool_calls"]
        if call["agent"] is not None
    )
    assert agent["id"] == "a-explore-1"


def test_as_dict_preserves_python_values():
    session = list(iter_claude_sessions(FIXTURES))[0]
    data = session.as_dict()
    assert isinstance(data["messages"][0]["timestamp"], datetime)


def test_as_json_round_trips_through_stdlib_json():
    session = list(iter_claude_sessions(FIXTURES))[0]
    parsed = json.loads(session.as_json())
    assert parsed["id"] == "s-main-1"
    assert parsed["messages"][0]["text"] == "fix the bug"


def test_all_models_have_json_methods():
    values = [
        Usage(input=1),
        ToolCall(id="call", name="Read", input={}),
        Message(role="user", timestamp=None),
        Session(id="session"),
        ProjectSummary(project="/repo"),
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
