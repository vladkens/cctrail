from datetime import UTC, datetime
from pathlib import Path

import pytest

from cctrail.config import Config, ConfigError, config_path, match_path
from cctrail.metrics import by_project
from cctrail.models import Message, Session, Usage


def test_config_path_prefers_local_file_then_global_config(tmp_path: Path, monkeypatch):
    workdir = tmp_path / "project"
    workdir.mkdir()
    global_config = tmp_path / "global"
    monkeypatch.chdir(workdir)
    monkeypatch.setenv("XDG_CONFIG_HOME", str(global_config))

    assert config_path() == global_config / "cctrail.toml"

    local = workdir / "cctrail.toml"
    local.touch()
    assert config_path() == local

    custom = tmp_path / "custom.toml"
    monkeypatch.setenv("CCTRAIL_CONFIG", str(custom))
    assert config_path() == custom


def test_missing_config_has_no_aliases(tmp_path: Path):
    assert Config.load(tmp_path / "missing.toml") == Config()


def test_config_resolves_exact_and_wildcard_paths(tmp_path: Path):
    path = tmp_path / "config.toml"
    path.write_text(
        '[aliases]\napp = ["~/Code/app", "/work/app--*"]\n',
        encoding="utf-8",
    )

    config = Config.load(path)
    assert config.resolve_project(str(Path.home() / "Code" / "app")) == "app"
    assert config.resolve_project("/work/app--feature") == "app"
    assert config.resolve_project("/other") == "/other"


@pytest.mark.parametrize(
    ("pattern", "path", "matches"),
    [
        ("/repo", "/repo", True),
        ("/repo", "/other", False),
        ("/Users/*/Code/cctrail", "/Users/alice/Code/cctrail", True),
        ("/Users/*/Code/cctrail", "/Users/alice/work/Code/cctrail", False),
        ("/Users/**/cctrail", "/Users/cctrail", True),
        ("/Users/**/cctrail", "/Users/alice/Code/cctrail", True),
        ("/**/cctrail", "/cctrail", True),
        ("/**/cctrail", "/Users/alice/cctrail", True),
        ("/Users/**", "/Users/alice/Code/cctrail", True),
        ("/Users/**", "/Users", True),
        ("**", "/Users/alice/Code/cctrail", True),
        ("**/acme/**", "/Users/alice/Code/acme", True),
        ("**/acme/**", "/Users/alice/Code/acme/acme-app", True),
        ("**/acme/**", "/Users/alice/Code/other/acme-app", False),
        ("/work/cctrail--*", "/work/cctrail--feature", True),
        ("/work/cctrail--*", "/work/cctrail--", True),
        ("/work/cctrail--*", "/work/cctrail--feature/nested", False),
        ("/work/*/project-*", "/work/client/project-api", True),
        ("/work/*/project-*", "/work/client/archive/project-api", False),
        ("/work/**/project-*", "/work/client/archive/project-api", True),
        ("/work/project.v1+dev", "/work/project.v1+dev", True),
        ("/work/project.v1+dev", "/work/projectXv11dev", False),
        ("/work/project?", "/work/project?", True),
        ("/work/project?", "/work/project1", False),
        ("/work/project[1]", "/work/project[1]", True),
        ("/work/project[1]", "/work/project1", False),
        ("/work/client project-*", "/work/client project-one", True),
        ("/работа/проект-*", "/работа/проект-тест", True),
    ],
)
def test_path_globs_follow_playwright_star_semantics(pattern: str, path: str, matches: bool):
    assert match_path(path, pattern) is matches


def test_config_normalizes_alias_and_project_paths(tmp_path: Path):
    path = tmp_path / "config.toml"
    path.write_text(
        '[aliases]\nrepo = ["/work/app/../repo/"]\n',
        encoding="utf-8",
    )

    config = Config.load(path)
    assert config.resolve_project("/work/repo/") == "repo"
    assert config.resolve_project("/work/other/../repo") == "repo"


def test_config_rejects_paths_in_multiple_aliases(tmp_path: Path):
    path = tmp_path / "config.toml"
    path.write_text(
        '[aliases]\none = ["/repo"]\ntwo = ["/repo"]\n',
        encoding="utf-8",
    )

    with pytest.raises(ConfigError, match="multiple aliases"):
        Config.load(path)


def test_resolve_rejects_ambiguous_wildcards(tmp_path: Path):
    path = tmp_path / "config.toml"
    path.write_text(
        '[aliases]\none = ["/work/*"]\ntwo = ["/work/app*"]\n',
        encoding="utf-8",
    )

    with pytest.raises(ConfigError, match="multiple aliases"):
        Config.load(path).resolve_project("/work/application")


def test_project_alias_merges_usage_and_overlapping_active_time(tmp_path: Path):
    path = tmp_path / "config.toml"
    path.write_text('[aliases]\napp = ["/repo", "/worktree"]\n', encoding="utf-8")
    stamp = datetime(2026, 1, 1, tzinfo=UTC)
    sessions = [
        Session(
            id=project,
            cwd=project,
            messages=[
                Message(
                    role="assistant",
                    timestamp=stamp,
                    usage=Usage(input=10),
                    model="unknown",
                )
            ],
        )
        for project in ("/repo", "/worktree")
    ]

    summary = by_project(sessions, project_resolver=Config.load(path).resolve_project)["app"]
    assert summary.session_count == 2
    assert summary.usage.total == 20
    assert summary.active_time.total_seconds() == 300
