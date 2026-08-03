from pathlib import Path

from cctrail.cli import main

FIXTURES = Path(__file__).parent / "fixtures" / "claude" / "projects"


def test_usage_command_runs(capsys):
    assert main(["usage", "--root", str(FIXTURES)]) == 0
    out = capsys.readouterr().out
    assert "2026-01-01" in out
    assert "total" in out
    # 2026-01-02 has only a zero-usage API-error turn; must not print as a row.
    assert "2026-01-02" not in out


def test_projects_command_runs(capsys):
    assert main(["projects", "--root", str(FIXTURES)]) == 0
    out = capsys.readouterr().out
    assert "/repo" in out


def test_projects_command_combines_providers(capsys):
    fixtures = Path(__file__).parent / "fixtures"
    assert main(["projects", "--root", str(fixtures)]) == 0
    out = capsys.readouterr().out
    assert "/repo" in out
    assert "962" in out


def test_keyboard_interrupt_exits_without_a_traceback(monkeypatch):
    def interrupted(_root):
        raise KeyboardInterrupt

    monkeypatch.setattr("cctrail.cli.iter_sessions", interrupted)
    assert main(["usage"]) == 130
