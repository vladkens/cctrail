import json
from datetime import date, timedelta
from pathlib import Path

from cctrail import iter_claude_sessions, tool_call_counts
from cctrail.metrics import active_time, usage_by_day

FIXTURES = Path(__file__).parent / "fixtures" / "claude" / "projects"
DEDUPE_FIXTURES = Path(__file__).parent / "fixtures" / "claude_dedupe" / "projects"


def test_iter_sessions_parses_single_session():
    sessions = list(iter_claude_sessions(FIXTURES))
    assert len(sessions) == 1

    session = sessions[0]
    assert session.id == "s-main-1"
    assert session.title == "Fix the bug"
    assert session.cwd == "/repo"
    assert session.git_branch == "main"
    assert session.provider == "claude"
    assert session.kind == "main"


def test_messages_merge_content_blocks_and_skip_synthetic_ones():
    session = list(iter_claude_sessions(FIXTURES))[0]
    roles = [m.role for m in session.messages]
    assert roles == ["user", "assistant", "assistant", "assistant", "assistant"]
    assert session.messages[0].text == "fix the bug"
    assert session.messages[0].turn_duration == timedelta(milliseconds=9100)


def test_api_error_messages_are_flagged_not_dropped():
    session = list(iter_claude_sessions(FIXTURES))[0]
    assert [m.is_api_error for m in session.messages] == [False, False, False, False, True]
    usage = session.messages[-1].usage
    assert usage is not None
    assert usage.total == 0


def test_usage_is_summed_per_logical_turn_not_per_line():
    session = list(iter_claude_sessions(FIXTURES))[0]
    assert session.usage.total == 135 + 57 + 161
    assert session.usage.cache_read == 100 + 50 + 150


def test_tool_calls_and_counts():
    session = list(iter_claude_sessions(FIXTURES))[0]
    assert tool_call_counts(session) == {"Bash": 1, "Agent": 1}
    bash_call = next(c for c in session.iter_tool_calls() if c.name == "Bash")
    assert bash_call.output_text == "file1\nfile2"
    assert bash_call.is_error is False
    assert bash_call.duration == timedelta(seconds=1)


def test_agent_tool_call_resolves_nested_session():
    session = list(iter_claude_sessions(FIXTURES))[0]
    agents = list(session.iter_agents())
    assert len(agents) == 1

    agent = agents[0]
    assert agent.kind == "agent"
    assert agent.id == "a-explore-1"
    assert agent.agent_type == "Explore"
    assert agent.title == "investigate"
    assert agent.cwd == "/repo"

    # Usage comes from the agent's own transcript, not the parent's summary.
    assert agent.usage.total == (1 + 0 + 20 + 10) + (1 + 0 + 30 + 12)

    # Wall-clock duration is Claude Code's authoritative figure, not derived
    # from message timestamps (which would give 4.8s here, not 5s).
    assert agent.duration == timedelta(seconds=5)
    assert tool_call_counts(agent) == {"Read": 1}


def test_walk_yields_main_session_and_every_nested_agent():
    session = list(iter_claude_sessions(FIXTURES))[0]
    assert [s.id for s in session.walk()] == ["s-main-1", "a-explore-1"]


def test_active_time_merges_overlapping_windows():
    session = list(iter_claude_sessions(FIXTURES))[0]
    events = [m.timestamp for m in session.messages if m.timestamp and not m.is_api_error]
    # All 4 real messages sit within ~10s of each other, well under the 5-minute window.
    assert active_time(events) == timedelta(minutes=5, seconds=10)


def test_unlinked_sidechains_are_parsed_but_usage_is_counted_once():
    sessions = list(iter_claude_sessions(DEDUPE_FIXTURES))

    assert len(sessions) == 3
    sidechain = next(session for session in sessions if session.id == "side")
    assert sidechain.kind == "agent"
    assert sidechain.usage.total == 1600 + 38

    buckets = usage_by_day(sessions)
    assert buckets[date(2026, 1, 4)].total == 16 + 26 + 38


def test_nested_subagent_transcript_uses_its_metadata(tmp_path: Path):
    project = tmp_path / "project"
    main_path = project / "session.jsonl"
    agent_dir = project / "session" / "subagents" / "workflows" / "run"
    agent_path = agent_dir / "agent-nested.jsonl"
    agent_dir.mkdir(parents=True)

    main_entries = [
        {
            "type": "assistant",
            "message": {
                "id": "message",
                "content": [{"type": "tool_use", "id": "tool", "name": "Agent", "input": {}}],
                "usage": {},
            },
            "timestamp": "2026-01-01T00:00:00Z",
            "sessionId": "session",
        },
        {
            "type": "user",
            "message": {"content": [{"type": "tool_result", "tool_use_id": "tool"}]},
            "toolUseResult": "User rejected tool use",
            "timestamp": "2026-01-01T00:00:01Z",
            "sessionId": "session",
        },
    ]
    main_path.write_text("\n".join(map(json.dumps, main_entries)), encoding="utf-8")
    agent_path.write_text(
        json.dumps(
            {
                "type": "assistant",
                "message": {"id": "answer", "content": [], "usage": {"output_tokens": 1}},
                "timestamp": "2026-01-01T00:00:00Z",
            }
        ),
        encoding="utf-8",
    )
    agent_path.with_suffix(".meta.json").write_text(
        json.dumps({"agentType": "Explore", "description": "Nested research", "toolUseId": "tool"}),
        encoding="utf-8",
    )

    sessions = list(iter_claude_sessions(tmp_path))

    assert len(sessions) == 1
    agent = next(sessions[0].iter_agents())
    assert agent.agent_type == "Explore"
    assert agent.title == "Nested research"


def test_malformed_structured_fields_do_not_abort_the_file(tmp_path: Path):
    path = tmp_path / "session.jsonl"
    entries = [
        {"type": "assistant", "message": "invalid"},
        {
            "type": "assistant",
            "message": {
                "id": "answer",
                "content": [{"type": "tool_use", "id": {}, "name": "Read", "input": []}],
                "usage": {"input_tokens": {}},
            },
        },
        {
            "type": "user",
            "message": {"content": [{"type": "text", "text": "hello"}]},
        },
    ]
    path.write_text("\n".join(map(json.dumps, entries)), encoding="utf-8")

    session = list(iter_claude_sessions(tmp_path))[0]
    assert [message.role for message in session.messages] == ["assistant", "user"]
    assert list(session.iter_tool_calls()) == []
    assert session.usage.total == 0
