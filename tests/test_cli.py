import os
import subprocess
import sys
from pathlib import Path

import orjson
import pytest

FIXTURES = Path(__file__).parent / "fixtures"


def run(
    claude: str,
    codex: str,
    *args: str,
    config: str = "empty.toml",
) -> subprocess.CompletedProcess[str]:
    env = os.environ | {
        "CLAUDE_CONFIG_DIR": str(FIXTURES / claude),
        "CODEX_HOME": str(FIXTURES / codex),
        "CCTRAIL_CONFIG": str(FIXTURES / config),
    }
    return subprocess.run(
        [sys.executable, "-m", "cctrail.cli", *args],
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


def json(result: subprocess.CompletedProcess[str]):
    assert result.returncode == 0, result.stderr
    return orjson.loads(result.stdout)


def total_tokens(stdout: str) -> str:
    line = next(x for x in stdout.splitlines() if x.startswith("total"))
    return line.split()[3]


def test_reports_claude_usage():
    result = run("claude/projects", "missing", "usage", "--provider", "claude")

    assert result.returncode == 0
    assert "2026-01-01" in result.stdout
    assert "2026-01-02" not in result.stdout
    assert total_tokens(result.stdout) == "427"


def test_reports_codex_usage_without_replay():
    result = run("missing", "codex", "usage", "--provider", "codex")

    assert result.returncode == 0
    assert total_tokens(result.stdout) == "455"


def test_deduplicates_claude_sidechains():
    result = run("claude_dedupe/projects", "missing", "usage", "--provider", "claude")

    assert result.returncode == 0
    assert total_tokens(result.stdout) == "80"


@pytest.mark.parametrize(
    "args",
    [
        ("--no-config", "stats", "--provider", "claude", "--json"),
        ("stats", "--no-config", "--provider", "claude", "--json"),
    ],
)
def test_no_config_is_global(args: tuple[str, ...]):
    value = json(run("claude/projects", "missing", *args, config="missing.toml"))

    assert value["query"]["by"] == "workdir"
    assert value["groups"][0]["workdir"] == "/repo"
    assert value["groups"][0]["usage"]["total"] == 427


def test_reports_configured_projects():
    result = run(
        "claude/projects",
        "missing",
        "stats",
        "--provider",
        "claude",
        config="config.toml",
    )

    assert result.returncode == 0
    assert any(x.startswith("oss/demo") and "427" in x for x in result.stdout.splitlines())


def test_reports_unmapped_section_total():
    result = run("claude/projects", "missing", "stats", "--provider", "claude")

    assert result.returncode == 0
    assert any(x.startswith("@unmapped") and "427" in x for x in result.stdout.splitlines())
    assert "1 unmapped workdir · run cctrail stats --show-unmapped to list them" in result.stdout
    assert not any(x.startswith("/repo") for x in result.stdout.splitlines())


def test_shows_unmapped_workdirs_on_request():
    result = run(
        "claude/projects",
        "missing",
        "stats",
        "--provider",
        "claude",
        "--show-unmapped",
    )

    assert result.returncode == 0
    assert "Unmapped workdirs (1)" in result.stdout
    assert any(x.startswith("/repo") and "0h 05m" in x for x in result.stdout.splitlines())


def test_unmapped_json_is_always_complete():
    value = json(run("claude/projects", "missing", "stats", "--provider", "claude", "--json"))

    assert value["groups"][0]["group"] == "@unmapped"
    assert value["groups"][0]["project"] == "/repo"
    assert value["groups"][0]["usage"]["total"] == 427
    assert value["groups"][0]["active_seconds"] == 310


def test_rejects_reversed_date_range():
    result = run(
        "missing",
        "missing",
        "usage",
        "--since",
        "2026-01-02",
        "--until",
        "2026-01-01",
    )

    assert result.returncode == 2
    assert "--since must not be later" in result.stderr


def test_usage_json_with_models():
    value = json(
        run(
            "claude/projects",
            "missing",
            "usage",
            "--provider",
            "claude",
            "--models",
            "--json",
        )
    )

    assert value["query"] == {
        "period": None,
        "since": None,
        "until": None,
        "provider": "claude",
        "granularity": "day",
        "models": True,
    }
    assert value["periods"][0]["usage"]["total"] == 427
    assert value["periods"][0]["cost_usd"] == 0.0005495
    assert value["periods"][0]["active_seconds"] == 310
    assert [x["model"] for x in value["periods"][0]["models"]] == [
        "claude-sonnet-5",
        "claude-haiku-4-5",
    ]


def test_codex_usage_json():
    value = json(
        run(
            "missing",
            "codex",
            "usage",
            "--provider",
            "codex",
            "--models",
            "--json",
        )
    )

    assert value["total"]["usage"]["total"] == 455
    assert value["total"]["active_seconds"] == 615
    assert value["total"]["cost_usd"] == 0.001401625
    assert [x["usage"]["total"] for x in value["periods"][0]["models"]] == [435, 20]


def test_stats_json():
    value = json(run("", "", "stats", "--json", config="config.toml"))

    assert value["command"] == "stats"
    assert value["query"]["by"] == "project"
    assert len(value["groups"]) == 1
    assert value["groups"][0]["group"] == "oss"
    assert value["groups"][0]["project"] == "demo"
    assert value["groups"][0]["sessions"] == 5
    assert value["groups"][0]["usage"]["total"] == 962
    assert value["groups"][0]["active_seconds"] == 1227
    assert value["total"]["projects"] == 1


def test_stats_by_group():
    value = json(run("", "", "stats", "--by", "group", "--json", config="config.toml"))

    assert value["groups"][0]["group"] == "oss"
    assert value["groups"][0]["usage"]["total"] == 962
    assert value["total"]["groups"] == 1


def test_months_clip_to_custom_range_without_models():
    value = json(
        run(
            "claude/projects",
            "missing",
            "usage",
            "--provider",
            "claude",
            "--by",
            "month",
            "--since",
            "2026-01-01",
            "--until",
            "2026-01-20",
            "--json",
        )
    )

    assert value["query"]["granularity"] == "month"
    assert value["periods"][0]["start"] == "2026-01-01"
    assert value["periods"][0]["end"] == "2026-01-20"
    assert value["periods"][0]["models"] == []


def test_weeks_start_on_monday():
    value = json(
        run(
            "claude/projects",
            "missing",
            "usage",
            "--provider",
            "claude",
            "--by",
            "week",
            "--json",
        )
    )

    assert value["periods"][0]["start"] == "2025-12-29"
    assert value["periods"][0]["end"] == "2026-01-04"


@pytest.mark.parametrize("flag", ["-V", "--version"])
def test_version(flag: str):
    result = run("missing", "missing", flag)

    assert result.returncode == 0
    assert result.stdout == "cctrail 0.2.0\n"


def test_help_lists_pricing_update():
    result = run("missing", "missing", "--help")

    assert result.returncode == 0
    assert "update-pricing" in result.stdout
