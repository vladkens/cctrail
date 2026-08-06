from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .models import JsonTrait, Usage
from .utils import json_load

PROVIDERS = {"claude": "anthropic", "codex": "openai"}
PRICE_FIELDS = ("input", "output", "cache_read", "cache_write")


@dataclass
class Pricing(JsonTrait):
    """Model prices in USD per million tokens."""

    input: float
    output: float
    cache_read: float | None = None
    cache_write: float | None = None
    long_context_threshold: int | None = None
    long_context_input: float | None = None
    long_context_output: float | None = None
    long_context_cache_read: float | None = None
    long_context_cache_write: float | None = None
    fast_multiplier: float = 1.0


def number(value: Any) -> int | float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return value


def pricing_from_model(model: dict[str, Any]) -> dict[str, Any] | None:
    cost = model.get("cost")
    if not isinstance(cost, dict):
        return None

    input_price = number(cost.get("input"))
    output_price = number(cost.get("output"))
    if input_price is None or output_price is None:
        return None

    result = {"input": input_price, "output": output_price}
    for field in ("cache_read", "cache_write"):
        if (value := number(cost.get(field))) is not None:
            result[field] = value

    tiers = cost.get("tiers")
    if isinstance(tiers, list):
        for tier in tiers:
            condition = tier.get("tier") if isinstance(tier, dict) else None
            if not isinstance(condition, dict) or condition.get("type") != "context":
                continue
            threshold = number(condition.get("size"))
            if threshold is None or int(threshold) != threshold:
                continue
            result["long_context_threshold"] = int(threshold)
            for field in PRICE_FIELDS:
                if (value := number(tier.get(field))) is not None:
                    result[f"long_context_{field}"] = value
            break

    experimental = model.get("experimental")
    modes = experimental.get("modes") if isinstance(experimental, dict) else None
    fast = modes.get("fast") if isinstance(modes, dict) else None
    fast_cost = fast.get("cost") if isinstance(fast, dict) else None
    fast_input = number(fast_cost.get("input")) if isinstance(fast_cost, dict) else None
    if fast_input is not None and input_price:
        result["fast_multiplier"] = fast_input / input_price

    return result


def catalog_from_models_dev(
    raw: Any, providers: tuple[str, ...] = ("anthropic", "openai")
) -> dict[str, dict[str, dict[str, Any]]]:
    if not isinstance(raw, dict):
        raise ValueError("models.dev response must be an object")

    result = {}
    for provider in providers:
        data = raw.get(provider)
        models = data.get("models") if isinstance(data, dict) else None
        if not isinstance(models, dict):
            raise ValueError(f"models.dev response has no {provider} models")

        entries = {}
        for key, model in models.items():
            if not isinstance(model, dict):
                continue
            model_id = model.get("id", key)
            pricing = pricing_from_model(model)
            if isinstance(model_id, str) and pricing is not None:
                entries[model_id] = pricing
        result[provider] = dict(sorted(entries.items()))

    return result


def load_catalog() -> dict[str, dict[str, Pricing]]:
    path = Path(__file__).with_name("pricing.json")
    raw = json_load(path.read_bytes(), {})
    if not isinstance(raw, dict):
        return {}

    result: dict[str, dict[str, Pricing]] = {}
    for provider, models in raw.items():
        if not isinstance(provider, str) or not isinstance(models, dict):
            continue
        result[provider] = {
            model: Pricing(**values)
            for model, values in models.items()
            if isinstance(model, str) and isinstance(values, dict)
        }
    return result


CATALOG = load_catalog()


def get_pricing(model: str, provider: str = "anthropic") -> Pricing | None:
    provider = PROVIDERS.get(provider, provider)
    return CATALOG.get(provider, {}).get(model)


def calculate_cost(
    model: str,
    usage: Usage,
    provider: str = "anthropic",
    speed: str | None = None,
) -> float:
    pricing = get_pricing(model, provider)
    if pricing is None:
        return 0.0

    input_rate = pricing.input
    output_rate = pricing.output
    cache_read_rate = pricing.cache_read if pricing.cache_read is not None else input_rate
    cache_write_rate = pricing.cache_write if pricing.cache_write is not None else input_rate * 1.25

    input_tokens = usage.input + usage.cache_read + usage.cache_write
    if pricing.long_context_threshold is not None and input_tokens > pricing.long_context_threshold:
        input_rate = (
            pricing.long_context_input if pricing.long_context_input is not None else input_rate
        )
        output_rate = (
            pricing.long_context_output if pricing.long_context_output is not None else output_rate
        )
        if pricing.long_context_cache_read is not None:
            cache_read_rate = pricing.long_context_cache_read
        elif pricing.cache_read is None:
            cache_read_rate = input_rate
        if pricing.long_context_cache_write is not None:
            cache_write_rate = pricing.long_context_cache_write
        elif pricing.cache_write is None:
            cache_write_rate = input_rate * 1.25

    cost = (
        usage.input * input_rate
        + usage.cache_write_5m * cache_write_rate
        + usage.cache_write_1h * input_rate * 2
        + usage.cache_read * cache_read_rate
        + (usage.output + usage.reasoning) * output_rate
    )
    multiplier = pricing.fast_multiplier if speed == "fast" else 1.0
    return cost * multiplier / 1_000_000
