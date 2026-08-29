from pathlib import Path

import orjson

from cctrail.metrics import DateRange, Granularity, usage_by_period
from cctrail.model import Provider
from cctrail.parser import claude, codex

FIXTURES = Path(__file__).parent / "fixtures"


def total(sessions) -> int:
    return sum(
        event.usage.total
        for session in sessions
        for event in session.events
        if event.usage is not None
    )


def test_claude_links_subagent_to_main_root():
    sessions = claude.parse(FIXTURES / "claude" / "projects")
    main = next(x for x in sessions if x.is_main)
    agent = next(x for x in sessions if not x.is_main)

    assert main.provider is Provider.CLAUDE
    assert agent.root_id == main.id
    assert agent.workdir == main.workdir == "/repo"


def test_codex_links_only_spawned_agent_and_removes_fork_replay():
    sessions = codex.parse(FIXTURES / "codex")
    main = next(x for x in sessions if x.id == "codex-main-1")
    agent = next(x for x in sessions if x.id == "codex-agent-1")
    fork = next(x for x in sessions if x.id == "codex-fork-1")

    assert main.provider is Provider.CODEX
    assert agent.root_id == main.id
    assert agent.workdir == main.workdir == "/repo"
    assert fork.root_id == fork.id
    assert sum(x.usage.total for x in fork.events if x.usage) == 25
    assert total(sessions) == 455


def test_claude_sidechain_fixture_keeps_eighty_tokens():
    sessions = claude.parse(FIXTURES / "claude_dedupe" / "projects")
    summaries = usage_by_period(sessions, Granularity.DAY, DateRange())

    assert sum(x.usage.total for x in summaries.values()) == 80


def test_claude_uses_tool_result_when_agent_log_is_missing(tmp_path: Path):
    lines = [
        {
            "type": "assistant",
            "timestamp": "2026-01-01T00:00:00Z",
            "cwd": "/repo",
            "message": {
                "content": [
                    {"type": "tool_use", "id": "tool", "name": "Agent"},
                ],
                "usage": {},
            },
        },
        {
            "type": "user",
            "timestamp": "2026-01-01T00:00:01Z",
            "message": {
                "content": [{"type": "tool_result", "tool_use_id": "tool"}],
            },
            "toolUseResult": {
                "agentId": "agent",
                "usage": {"input_tokens": 2, "output_tokens": 3},
            },
        },
    ]
    (tmp_path / "main.jsonl").write_bytes(b"\n".join(orjson.dumps(x) for x in lines))

    sessions = claude.parse(tmp_path)
    main = next(x for x in sessions if x.is_main)
    agent = next(x for x in sessions if not x.is_main)

    assert agent.root_id == main.id
    assert agent.workdir == "/repo"
    assert total([agent]) == 5
