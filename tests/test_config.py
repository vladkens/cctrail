from pathlib import Path

import pytest

from cctrail.config import Config, StatsBy


def test_maps_descendants_by_longest_prefix():
    config = Config.parse(
        """
        [projects]
        "oss/tools" = ["~/Code/tools"]
        "try/sandbox" = ["~/Code/tools/sandbox"]
        """,
        Path("/home/me"),
    )

    assert config.map_workdir("/home/me/Code/tools/cli", StatsBy.PROJECT) == "oss/tools"
    assert config.map_workdir("/home/me/Code/tools/sandbox/app", StatsBy.PROJECT) == "try/sandbox"
    assert config.map_workdir("/other", StatsBy.GROUP) == "@unmapped"
    assert config.map_workdir("/other", StatsBy.PROJECT) == "/other"


@pytest.mark.parametrize(
    ("text", "message"),
    [
        ("[projects]\ninvalid = ['/repo']", "<group>/<project>"),
        ("[projects]\n'oss/demo' = []", "has no paths"),
        ("[projects]\n'oss/demo' = ['relative']", "must be absolute"),
        (
            "[projects]\n'oss/one' = ['/repo']\n'try/two' = ['/repo']",
            "belongs to both",
        ),
    ],
)
def test_rejects_invalid_config(text: str, message: str):
    with pytest.raises(ValueError, match=message):
        Config.parse(text)


def test_explicit_config_must_exist(monkeypatch: pytest.MonkeyPatch, tmp_path: Path):
    monkeypatch.setenv("CCTRAIL_CONFIG", str(tmp_path / "missing.toml"))

    with pytest.raises(ValueError, match="failed to read"):
        Config.load()


def test_automatic_config_is_optional(monkeypatch: pytest.MonkeyPatch, tmp_path: Path):
    monkeypatch.delenv("CCTRAIL_CONFIG", raising=False)
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))

    assert Config.load() == Config()
