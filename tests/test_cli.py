import json
from pathlib import Path

from cctrail.cli import main

FIXTURES = Path(__file__).parent / "fixtures" / "claude" / "projects"


def test_usage_command_runs(capsys):
    assert main(["usage", "--root", str(FIXTURES)]) == 0
    out = capsys.readouterr().out
    assert "2026-01-01" in out
    assert "total" in out
    assert "Cost" in out
    assert "$" in out
    # 2026-01-02 has only a zero-usage API-error turn; must not print as a row.
    assert "2026-01-02" not in out


def test_projects_command_runs(capsys):
    assert main(["projects", "--root", str(FIXTURES)]) == 0
    out = capsys.readouterr().out
    assert "/repo" in out
    assert "Cost" in out


def test_projects_command_combines_providers(capsys):
    fixtures = Path(__file__).parent / "fixtures"
    assert main(["projects", "--root", str(fixtures)]) == 0
    out = capsys.readouterr().out
    assert "/repo" in out
    assert "962" in out


def test_audit_command_reports_complete_pricing(capsys):
    assert main(["audit", "--root", str(FIXTURES)]) == 0
    out = capsys.readouterr().out
    assert "Pricing audit" in out
    assert "cctrail version" in out
    assert "No pricing issues found." in out


def test_audit_command_reports_unpriced_usage(tmp_path: Path, capsys):
    path = tmp_path / "session.jsonl"
    path.write_text(
        json.dumps(
            {
                "type": "assistant",
                "message": {
                    "id": "message",
                    "model": "unknown-model",
                    "content": [],
                    "usage": {"input_tokens": 10, "output_tokens": 5},
                },
                "timestamp": "2026-01-01T00:00:00Z",
            }
        ),
        encoding="utf-8",
    )

    assert main(["audit", "--root", str(tmp_path)]) == 0
    out = capsys.readouterr().out
    assert "missing pricing" in out
    assert "unknown-model" in out
    assert "15" in out
    assert str(tmp_path) not in out


def test_keyboard_interrupt_exits_without_a_traceback(monkeypatch):
    def interrupted(_root):
        raise KeyboardInterrupt

    monkeypatch.setattr("cctrail.cli.iter_sessions", interrupted)
    assert main(["usage"]) == 130
