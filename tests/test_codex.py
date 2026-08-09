import json
from datetime import timedelta
from pathlib import Path

import pytest

import cctrail.codex as codex
from cctrail import iter_codex_sessions, tool_call_counts

FIXTURES = Path(__file__).parent / "fixtures" / "codex"


def test_iter_sessions_parses_codex_session():
    sessions = list(iter_codex_sessions(FIXTURES))
    assert len(sessions) == 2

    session = next(session for session in sessions if session.id == "codex-main-1")
    assert session.id == "codex-main-1"
    assert session.title == "Fix the Codex parser"
    assert session.cwd == "/repo"
    assert session.git_branch == "main"
    assert session.provider == "codex"
    assert session.kind == "main"


def test_messages_and_usage_are_normalized():
    session = next(
        session for session in iter_codex_sessions(FIXTURES) if session.id == "codex-main-1"
    )
    assert [message.role for message in session.messages] == [
        "user",
        "assistant",
        "assistant",
        "assistant",
    ]
    assert session.messages[0].text == "fix the parser"
    assert session.messages[0].turn_duration == timedelta(seconds=30)
    assert session.messages[1].model == "gpt-5.4"

    assert session.usage.input == 40 + 50 + 30
    assert session.usage.cache_read == 60 + 150 + 20
    assert session.usage.output == 15 + 20 + 8
    assert session.usage.reasoning == 5 + 10 + 2
    assert session.usage.total == 120 + 230 + 60


def test_tool_calls_parse_json_custom_input_and_output():
    session = next(
        session for session in iter_codex_sessions(FIXTURES) if session.id == "codex-main-1"
    )
    assert tool_call_counts(session) == {
        "exec_command": 1,
        "web_search": 1,
        "apply_patch": 1,
        "spawn_agent": 1,
    }

    command = next(call for call in session.iter_tool_calls() if call.name == "exec_command")
    assert command.input == {"cmd": "rg parser"}
    assert command.output_text == "parser.py"
    assert command.duration == timedelta(seconds=2)

    patch = next(call for call in session.iter_tool_calls() if call.name == "apply_patch")
    assert patch.input == {"value": "*** Begin Patch"}
    assert patch.output_text == "Done!"


def test_spawned_agent_is_attached_to_the_call():
    session = next(
        session for session in iter_codex_sessions(FIXTURES) if session.id == "codex-main-1"
    )
    assert [item.id for item in session.walk()] == ["codex-main-1", "codex-agent-1"]

    agent = next(session.iter_agents())
    assert agent.kind == "agent"
    assert agent.provider == "codex"
    assert agent.agent_type == "explorer"
    assert agent.title == "Scout"
    assert agent.usage.total == 20


def test_fork_is_a_root_session_without_replayed_history():
    fork = next(
        session for session in iter_codex_sessions(FIXTURES) if session.id == "codex-fork-1"
    )

    assert fork.kind == "main"
    assert fork.title == "Try another approach"
    assert fork.git_branch == "experiment"
    assert [message.role for message in fork.messages] == ["user", "assistant"]
    assert fork.messages[0].text == "try another approach"
    assert fork.usage.total == 25


def test_service_tier_is_kept_with_usage(tmp_path: Path):
    path = tmp_path / "session.jsonl"
    entries = [
        {"type": "session_meta", "payload": {"id": "session"}},
        {"type": "turn_context", "payload": {"model": "gpt-5.4"}},
        {
            "type": "event_msg",
            "payload": {
                "type": "thread_settings_applied",
                "thread_settings": {"service_tier": "priority"},
            },
        },
        {
            "type": "event_msg",
            "payload": {"type": "thread_settings_applied", "thread_settings": {}},
        },
        {
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "last_token_usage": {
                        "input_tokens": 100,
                        "cached_input_tokens": 60,
                        "output_tokens": 20,
                        "reasoning_output_tokens": 5,
                    }
                },
            },
        },
        {
            "type": "event_msg",
            "payload": {
                "type": "thread_settings_applied",
                "thread_settings": {"service_tier": "unknown"},
            },
        },
        {
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "last_token_usage": {
                        "input_tokens": 1,
                        "output_tokens": 1,
                    }
                },
            },
        },
    ]
    path.write_text("\n".join(map(json.dumps, entries)), encoding="utf-8")

    messages = list(iter_codex_sessions(tmp_path))[0].messages
    message = messages[0]
    assert message.speed == "fast"
    assert message.usage is not None
    assert message.usage.input == 40
    assert message.usage.cache_read == 60
    assert message.usage.output == 15
    assert message.usage.reasoning == 5
    assert messages[1].speed is None


def test_codex_home_combines_comma_separated_directories(tmp_path: Path, monkeypatch):
    roots = [tmp_path / "first", tmp_path / "second"]
    for index, root in enumerate(roots):
        path = root / "sessions" / f"session-{index}.jsonl"
        path.parent.mkdir(parents=True)
        entries = [
            {"type": "session_meta", "payload": {"id": f"session-{index}"}},
            {"type": "turn_context", "payload": {"model": "gpt-5.4"}},
            {
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {"last_token_usage": {"input_tokens": 1}},
                },
            },
        ]
        path.write_text("\n".join(map(json.dumps, entries)), encoding="utf-8")

    monkeypatch.setenv("CODEX_HOME", ",".join(map(str, roots)))

    assert {session.id for session in iter_codex_sessions()} == {"session-0", "session-1"}


def test_parallel_parser_matches_sequential_parser(monkeypatch):
    sequential = list(iter_codex_sessions(FIXTURES))

    monkeypatch.setattr(codex, "PARALLEL_MIN_FILES", 0)
    monkeypatch.setenv("CCTRAIL_WORKERS", "")
    parallel = list(iter_codex_sessions(FIXTURES))

    assert parallel == sequential


def test_one_worker_uses_current_process(monkeypatch):
    monkeypatch.setattr(codex, "PARALLEL_MIN_FILES", 0)
    monkeypatch.setenv("CCTRAIL_WORKERS", "1")

    def unexpected_pool(*_args, **_kwargs):
        raise AssertionError("multiprocessing context must not be created")

    monkeypatch.setattr("cctrail.utils.get_context", unexpected_pool)

    assert list(iter_codex_sessions(FIXTURES))


def test_worker_count_controls_pool_size(monkeypatch):
    monkeypatch.setattr(codex, "PARALLEL_MIN_FILES", 0)
    monkeypatch.setenv("CCTRAIL_WORKERS", "2")
    pool_sizes = []

    class Pool:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

        def imap(self, func, items, _chunksize):
            return map(func, items)

    class Context:
        def Pool(self, workers, initializer):
            pool_sizes.append(workers)
            assert initializer is not None
            return Pool()

    monkeypatch.setattr("cctrail.utils.get_context", lambda _method: Context())

    assert list(iter_codex_sessions(FIXTURES))
    assert pool_sizes == [2, 2]


@pytest.mark.parametrize("value", ["0", "-1", "nope"])
def test_worker_count_rejects_invalid_values(monkeypatch, value):
    monkeypatch.setattr(codex, "PARALLEL_MIN_FILES", 0)
    monkeypatch.setenv("CCTRAIL_WORKERS", value)

    with pytest.raises(ValueError, match="CCTRAIL_WORKERS must be a positive integer"):
        list(iter_codex_sessions(FIXTURES))
