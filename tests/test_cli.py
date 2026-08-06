import json
from pathlib import Path

from cctrail.cli import main

FIXTURES = Path(__file__).parent / "fixtures" / "claude" / "projects"


def set_data_dirs(monkeypatch, claude: str | Path, codex: str | Path | None = None):
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(claude))
    codex_home = codex if codex is not None else Path(claude) / "missing-codex"
    monkeypatch.setenv("CODEX_HOME", str(codex_home))


def write_claude_usage(root: Path, model: str):
    path = root / "projects" / model / "session.jsonl"
    path.parent.mkdir(parents=True)
    path.write_text(
        json.dumps(
            {
                "type": "assistant",
                "message": {
                    "id": model,
                    "model": model,
                    "content": [],
                    "usage": {"input_tokens": 10, "output_tokens": 5},
                },
                "timestamp": "2026-01-01T00:00:00Z",
            }
        ),
        encoding="utf-8",
    )


def test_usage_command_runs(capsys, monkeypatch):
    set_data_dirs(monkeypatch, FIXTURES)
    assert main(["usage"]) == 0
    out = capsys.readouterr().out
    assert "2026-01-01" in out
    assert "total" in out
    assert "Cost" in out
    assert "Time" in out
    assert "0h 05m" in out
    assert "Blocks" not in out
    assert "$" in out
    # 2026-01-02 has only a zero-usage API-error turn; must not print as a row.
    assert "2026-01-02" not in out


def test_projects_command_runs(capsys, monkeypatch):
    set_data_dirs(monkeypatch, FIXTURES)
    assert main(["projects"]) == 0
    out = capsys.readouterr().out
    assert "/repo" in out
    assert "Cost" in out
    assert "Time" in out
    assert "0h 05m" in out
    assert "Blocks" not in out


def test_projects_command_combines_providers(capsys, monkeypatch):
    fixtures = Path(__file__).parent / "fixtures"
    set_data_dirs(monkeypatch, fixtures, fixtures)
    assert main(["projects"]) == 0
    out = capsys.readouterr().out
    assert "/repo" in out
    assert "962" in out


def test_audit_command_reports_complete_pricing(capsys, monkeypatch):
    set_data_dirs(monkeypatch, FIXTURES)
    assert main(["audit"]) == 0
    out = capsys.readouterr().out
    assert "Pricing audit" in out
    assert "cctrail version" in out
    assert "No pricing issues found." in out


def test_audit_command_reports_unpriced_usage(tmp_path: Path, capsys, monkeypatch):
    write_claude_usage(tmp_path, "unknown-model")
    set_data_dirs(monkeypatch, tmp_path)

    assert main(["audit"]) == 0
    out = capsys.readouterr().out
    assert "missing pricing" in out
    assert "unknown-model" in out
    assert "15" in out
    assert str(tmp_path) not in out


def test_audit_command_combines_comma_separated_data_dirs(tmp_path: Path, capsys, monkeypatch):
    first = tmp_path / "first"
    second = tmp_path / "second"
    write_claude_usage(first, "unknown-one")
    write_claude_usage(second, "unknown-two")
    set_data_dirs(monkeypatch, f"{first},{second}")

    assert main(["audit"]) == 0
    out = capsys.readouterr().out
    assert "unknown-one" in out
    assert "unknown-two" in out


def test_keyboard_interrupt_exits_without_a_traceback(monkeypatch):
    def interrupted():
        raise KeyboardInterrupt

    monkeypatch.setattr("cctrail.cli.iter_sessions", interrupted)
    assert main(["usage"]) == 130
