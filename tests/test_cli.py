import calendar
import json
from datetime import date
from pathlib import Path

import pytest

from cctrail import ModelSummary, ProjectSummary
from cctrail.cli import main, model_label, period_label, period_range

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
    assert "Reasoning" not in out
    assert "Cache W" not in out
    assert "0h 05m" in out
    assert "Blocks" not in out
    assert "$" in out
    # 2026-01-02 has only a zero-usage API-error turn; must not print as a row.
    assert "2026-01-02" not in out


def test_usage_full_shows_detailed_token_columns(capsys, monkeypatch):
    set_data_dirs(monkeypatch, FIXTURES)
    assert main(["usage", "--full"]) == 0
    out = capsys.readouterr().out
    assert "Reasoning" in out
    assert "Cache W" in out
    assert "Cache R" in out


def test_usage_models_breaks_down_each_period(capsys, monkeypatch):
    set_data_dirs(monkeypatch, FIXTURES)
    assert main(["usage", "--models", "--full"]) == 0
    out = capsys.readouterr().out
    assert "Date / Model" in out
    assert "claude/claude-haiku" in out
    assert "claude/claude-sonnet" in out
    assert "…" not in out


def test_model_labels_do_not_inherit_the_period_color():
    assert model_label(ModelSummary(provider="codex", model="gpt-5.6-sol")).style == "default"


def test_usage_can_group_by_month(capsys, monkeypatch):
    set_data_dirs(monkeypatch, FIXTURES)
    assert main(["usage", "--by", "month"]) == 0
    out = capsys.readouterr().out
    assert "Month" in out
    assert "2026-01" in out
    assert "2026-01-01" not in out


def test_usage_clips_week_label_to_the_selected_range(capsys, monkeypatch):
    set_data_dirs(monkeypatch, FIXTURES)
    assert (
        main(
            [
                "usage",
                "--since",
                "2026-01-01",
                "--until",
                "2026-01-01",
                "--by",
                "week",
            ]
        )
        == 0
    )
    out = capsys.readouterr().out
    assert "2026-01-01" in out
    assert "2025-12-29" not in out
    assert "01-04" not in out


def test_projects_command_runs(capsys, monkeypatch):
    set_data_dirs(monkeypatch, FIXTURES)
    assert main(["projects"]) == 0
    out = capsys.readouterr().out
    assert "/repo" in out
    assert "Cost" in out
    assert "Time" in out
    assert "0h 05m" in out
    assert "Blocks" not in out


def test_projects_accepts_the_shared_date_filter(capsys, monkeypatch):
    set_data_dirs(monkeypatch, FIXTURES)
    assert main(["projects", "--since", "2026-01-02"]) == 0
    assert "/repo" not in capsys.readouterr().out


def test_projects_full_controls_long_path_truncation(capsys, monkeypatch):
    project = "/" + "very-long-project-path/" * 10
    summary = ProjectSummary(project=project)
    monkeypatch.setattr("cctrail.cli.iter_sessions", lambda: [])
    monkeypatch.setattr("cctrail.cli.by_project", lambda *args, **kwargs: {project: summary})

    assert main(["projects"]) == 0
    assert project not in capsys.readouterr().out

    assert main(["projects", "--full"]) == 0
    assert project in capsys.readouterr().out


def test_projects_command_combines_providers(capsys, monkeypatch):
    fixtures = Path(__file__).parent / "fixtures"
    set_data_dirs(monkeypatch, fixtures, fixtures)
    assert main(["projects"]) == 0
    out = capsys.readouterr().out
    assert "/repo" in out
    assert "962" in out


def test_projects_uses_configured_aliases(tmp_path, capsys, monkeypatch):
    set_data_dirs(monkeypatch, FIXTURES)
    config = tmp_path / "config.toml"
    config.write_text('[aliases]\napplication = ["/repo"]\n', encoding="utf-8")
    monkeypatch.setenv("CCTRAIL_CONFIG", str(config))

    assert main(["projects", "--json"]) == 0
    projects = json.loads(capsys.readouterr().out)["projects"]
    assert [project["project"] for project in projects] == ["application"]


def test_projects_reports_invalid_config(tmp_path, capsys, monkeypatch):
    set_data_dirs(monkeypatch, FIXTURES)
    config = tmp_path / "config.toml"
    config.write_text("invalid = [", encoding="utf-8")
    monkeypatch.setenv("CCTRAIL_CONFIG", str(config))

    assert main(["projects"]) == 2
    assert "config error:" in capsys.readouterr().err


@pytest.mark.parametrize("command", ["usage", "projects", "audit"])
def test_reports_can_filter_by_provider(command, capsys, monkeypatch):
    fixtures = Path(__file__).parent / "fixtures"
    set_data_dirs(monkeypatch, fixtures, fixtures)

    assert main([command, "--provider", "claude"]) == 0
    claude = capsys.readouterr().out
    assert main([command, "--provider", "codex"]) == 0
    codex = capsys.readouterr().out

    assert claude != codex


@pytest.mark.parametrize("command", ["usage", "projects", "audit"])
def test_report_commands_support_json(command, capsys, monkeypatch):
    set_data_dirs(monkeypatch, FIXTURES)
    assert main([command, "--json"]) == 0
    captured = capsys.readouterr()
    payload = json.loads(captured.out)
    assert payload["schema_version"] == 1
    assert payload["command"] == command
    assert captured.out.endswith("\n")
    assert captured.err == ""


def test_usage_json_has_raw_values_and_optional_models(capsys, monkeypatch):
    set_data_dirs(monkeypatch, FIXTURES)
    assert main(["usage", "--models", "--json"]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["query"]["granularity"] == "day"
    assert payload["query"]["models"] is True
    assert payload["periods"][0]["usage"]["total"] == 427
    assert payload["periods"][0]["active_seconds"] == 310
    assert payload["periods"][0]["models"][0]["provider"] == "claude"
    assert payload["total"]["usage"]["total"] == 427


def test_usage_json_keeps_stable_models_shape(capsys, monkeypatch):
    set_data_dirs(monkeypatch, FIXTURES)
    assert main(["usage", "--json"]) == 0
    normal = json.loads(capsys.readouterr().out)
    assert all(period["models"] == [] for period in normal["periods"])

    assert main(["usage", "--full", "--json"]) == 0
    assert json.loads(capsys.readouterr().out) == normal


def test_usage_json_clips_bucket_dates(capsys, monkeypatch):
    set_data_dirs(monkeypatch, FIXTURES)
    assert (
        main(["usage", "--since", "2026-01-01", "--until", "2026-01-01", "--by", "week", "--json"])
        == 0
    )
    period = json.loads(capsys.readouterr().out)["periods"][0]
    assert period["start"] == "2026-01-01"
    assert period["end"] == "2026-01-01"


def test_projects_json_preserves_full_paths(capsys, monkeypatch):
    project = "/" + "very-long-project-path/" * 10
    summary = ProjectSummary(project=project)
    monkeypatch.setattr("cctrail.cli.iter_sessions", lambda: [])
    monkeypatch.setattr("cctrail.cli.by_project", lambda *args, **kwargs: {project: summary})
    assert main(["projects", "--json"]) == 0
    assert json.loads(capsys.readouterr().out)["projects"][0]["project"] == project


def test_audit_json_contains_structured_issues(tmp_path: Path, capsys, monkeypatch):
    write_claude_usage(tmp_path, "unknown-model")
    set_data_dirs(monkeypatch, tmp_path)
    assert main(["audit", "--json"]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["summary"]["unpriced_messages"] == 1
    assert payload["issues"] == [
        {
            "issue": "missing pricing",
            "provider": "claude",
            "model": "unknown-model",
            "messages": 1,
            "tokens": 15,
        }
    ]


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


def test_named_week_ranges_respect_the_locale_first_weekday():
    today = date(2026, 8, 5)
    assert period_range("last-week", today=today, first_weekday=calendar.MONDAY) == (
        date(2026, 7, 27),
        date(2026, 8, 2),
    )
    assert period_range("last-week", today=today, first_weekday=calendar.SUNDAY) == (
        date(2026, 7, 26),
        date(2026, 8, 1),
    )


@pytest.mark.parametrize(
    ("value", "granularity", "since", "until", "expected"),
    [
        (
            date(2026, 7, 27),
            "week",
            date(2026, 8, 1),
            date(2026, 8, 31),
            "2026-08-01 – 08-02",
        ),
        (
            date(2026, 8, 31),
            "week",
            date(2026, 8, 1),
            date(2026, 8, 31),
            "2026-08-31",
        ),
        (
            date(2026, 8, 1),
            "month",
            date(2026, 8, 5),
            date(2026, 8, 20),
            "2026-08-05 – 08-20",
        ),
        (
            date(2026, 8, 1),
            "month",
            None,
            None,
            "2026-08",
        ),
    ],
)
def test_period_label_shows_only_the_selected_range(
    value,
    granularity,
    since,
    until,
    expected,
):
    assert (
        period_label(
            value,
            granularity,
            since=since,
            until=until,
        )
        == expected
    )


def test_named_period_cannot_be_combined_with_manual_dates():
    with pytest.raises(SystemExit):
        main(["usage", "today", "--since", "2026-01-01"])
