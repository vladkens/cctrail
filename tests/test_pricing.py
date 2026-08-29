from dataclasses import asdict
from pathlib import Path

import orjson
import pytest

from cctrail import pricing
from cctrail.model import Provider, Speed, Usage
from cctrail.pricing import Price, catalog_from_models_dev, cost, load_catalog, update_catalog


def test_cost_prices_reasoning_fast_and_long_context():
    usage = Usage(input=272_001, output=500, reasoning=500)
    standard = cost(Provider.CODEX, "gpt-5.4", usage, Speed.STANDARD)
    fast = cost(Provider.CODEX, "gpt-5.4", usage, Speed.FAST)

    assert standard == 1.382505
    assert fast == standard * 2


def test_generates_catalog_from_models_dev():
    raw = {
        "anthropic": {
            "models": {
                "key": {
                    "id": "model",
                    "cost": {
                        "input": 2,
                        "output": 10,
                        "cache_read": 0.2,
                        "tiers": [
                            {
                                "input": 4,
                                "output": 15,
                                "cache_read": 0.4,
                                "tier": {"type": "context", "size": 200_000},
                            }
                        ],
                    },
                    "experimental": {"modes": {"fast": {"cost": {"input": 6}}}},
                }
            }
        },
        "openai": {"models": {}},
    }

    price = catalog_from_models_dev(raw)["anthropic"]["model"]

    assert asdict(price) == {
        "input": 2.0,
        "output": 10.0,
        "cache_read": 0.2,
        "cache_write": None,
        "long_context_threshold": 200_000,
        "long_context_input": 4.0,
        "long_context_output": 15.0,
        "long_context_cache_read": 0.4,
        "long_context_cache_write": None,
        "fast_multiplier": 3.0,
    }


@pytest.mark.parametrize("raw", [None, {"anthropic": {"models": {}}}])
def test_rejects_incomplete_catalog(raw):
    with pytest.raises(ValueError):
        catalog_from_models_dev(raw)


def test_update_catalog_writes_package_schema(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
):
    catalog = {
        "anthropic": {"model": Price(input=2, output=10, cache_read=0.2)},
        "openai": {},
    }
    monkeypatch.setattr("cctrail.pricing.download_catalog", lambda: catalog)
    path = tmp_path / "cache/pricing.json"

    update_catalog(path)

    assert orjson.loads(path.read_bytes()) == {
        "anthropic": {"model": {"input": 2, "output": 10, "cache_read": 0.2}},
        "openai": {},
    }
    assert not path.with_name(".pricing.json.tmp").exists()


def test_prefers_cached_catalog(monkeypatch: pytest.MonkeyPatch, tmp_path: Path):
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path))
    path = tmp_path / "cctrail/pricing.json"
    path.parent.mkdir()
    path.write_bytes(orjson.dumps({"openai": {"model": {"input": 2, "output": 10}}}))

    assert load_catalog()["openai"]["model"] == Price(input=2, output=10)


def test_falls_back_from_invalid_cache(monkeypatch: pytest.MonkeyPatch, tmp_path: Path):
    package = tmp_path / "pricing.json"
    package.write_bytes(orjson.dumps({"openai": {"model": {"input": 2, "output": 10}}}))
    cached = tmp_path / "cache/cctrail/pricing.json"
    cached.parent.mkdir(parents=True)
    cached.write_text("invalid")
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path / "cache"))
    monkeypatch.setattr(pricing, "CATALOG_PATH", package)

    assert load_catalog()["openai"]["model"] == Price(input=2, output=10)
