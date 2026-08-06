import runpy
from pathlib import Path

from cctrail import Usage, calculate_cost, get_pricing
from cctrail.pricing import catalog_from_models_dev

SCRIPT = runpy.run_path(str(Path(__file__).parents[1] / "scripts" / "update-pricing.py"))


def test_update_script_uses_library_catalog_transform():
    assert SCRIPT["catalog_from_models_dev"] is catalog_from_models_dev


def test_get_pricing_accepts_provider_and_history_names():
    assert get_pricing("claude-sonnet-5", provider="anthropic") == get_pricing(
        "claude-sonnet-5", provider="claude"
    )
    assert get_pricing("gpt-5.4", provider="openai") == get_pricing("gpt-5.4", provider="codex")
    assert get_pricing("unknown") is None


def test_calculate_cost_prices_all_claude_token_types():
    usage = Usage(
        input=1_000_000,
        cache_write_5m=1_000_000,
        cache_write_1h=1_000_000,
        cache_read=1_000_000,
        output=1_000_000,
        reasoning=1_000_000,
    )

    assert calculate_cost("claude-sonnet-5", usage) == 28.7


def test_calculate_cost_prices_reasoning_fast_and_long_context():
    usage = Usage(input=272_001, output=500, reasoning=500)
    standard = calculate_cost("gpt-5.4", usage, provider="codex")
    fast = calculate_cost("gpt-5.4", usage, provider="codex", speed="fast")

    assert standard == 1.382505
    assert fast == standard * 2


def test_catalog_from_models_dev_keeps_providers_tiers_and_fast_pricing():
    raw = {
        "anthropic": {
            "models": {
                "model": {
                    "cost": {
                        "input": 2,
                        "output": 10,
                        "cache_read": 0.2,
                        "cache_write": 2.5,
                        "tiers": [
                            {
                                "input": 4,
                                "output": 15,
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

    catalog = catalog_from_models_dev(raw)
    assert catalog == {
        "anthropic": {
            "model": {
                "input": 2,
                "output": 10,
                "cache_read": 0.2,
                "cache_write": 2.5,
                "long_context_threshold": 200_000,
                "long_context_input": 4,
                "long_context_output": 15,
                "fast_multiplier": 3,
            }
        },
        "openai": {},
    }


def test_catalog_changes_reports_only_real_model_changes():
    before = {
        "anthropic": {
            "changed": {"input": 4},
            "old": {"input": 2},
            "same": {"input": 1},
        }
    }
    after = {
        "anthropic": {
            "changed": {"input": 5},
            "new": {"input": 3},
            "same": {"input": 1},
        }
    }

    assert SCRIPT["catalog_changes"](before, before) == []
    assert SCRIPT["catalog_changes"](before, after) == [
        'changed anthropic/changed: {"input":4} → {"input":5}',
        'added anthropic/new: null → {"input":3}',
        'removed anthropic/old: {"input":2} → null',
    ]
