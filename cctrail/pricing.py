from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any
from urllib.request import Request, urlopen

import orjson

from .model import Provider, Speed, Usage

CATALOG_URL = "https://models.dev/api.json"
CATALOG_PATH = Path(__file__).with_name("pricing.json")


@dataclass
class Price:
    input: float
    output: float
    cache_read: float | None = None
    cache_write: float | None = None
    long_context_threshold: int | None = None
    long_context_input: float | None = None
    long_context_output: float | None = None
    long_context_cache_read: float | None = None
    long_context_cache_write: float | None = None
    fast_multiplier: float | None = None


Catalog = dict[str, dict[str, Price]]


def load_catalog(path: Path = CATALOG_PATH) -> Catalog:
    raw = orjson.loads(path.read_bytes())
    return {
        provider: {model: Price(**price) for model, price in models.items()}
        for provider, models in raw.items()
    }


CATALOG = load_catalog()


def cost(
    provider: Provider,
    model: str | None,
    usage: Usage,
    speed: Speed | None,
) -> float:
    price = CATALOG.get(provider.pricing_key, {}).get(model or "")
    if price is None:
        return 0.0

    long = (
        price.long_context_threshold is not None
        and usage.input + usage.cache_write + usage.cache_read > price.long_context_threshold
    )
    input_price = (
        price.long_context_input if long and price.long_context_input is not None else price.input
    )
    output_price = (
        price.long_context_output
        if long and price.long_context_output is not None
        else price.output
    )
    read = price.long_context_cache_read if long else None
    if read is None:
        read = price.cache_read if price.cache_read is not None else input_price
    write = price.long_context_cache_write if long else None
    if write is None:
        write = price.cache_write if price.cache_write is not None else input_price * 1.25
    dollars = (
        usage.input * input_price
        + usage.cache_write_5m * write
        + usage.cache_write_1h * input_price * 2
        + usage.cache_read * read
        + (usage.output + usage.reasoning) * output_price
    )
    multiplier = (
        price.fast_multiplier if speed is Speed.FAST and price.fast_multiplier is not None else 1.0
    )
    return dollars * multiplier / 1_000_000


def update_catalog(path: Path = CATALOG_PATH) -> None:
    catalog = download_catalog()
    value = {
        provider: {
            model: {key: value for key, value in asdict(price).items() if value is not None}
            for model, price in models.items()
        }
        for provider, models in catalog.items()
    }
    path.write_bytes(orjson.dumps(value, option=orjson.OPT_INDENT_2 | orjson.OPT_SORT_KEYS) + b"\n")


def download_catalog() -> Catalog:
    request = Request(CATALOG_URL, headers={"User-Agent": "cctrail"})
    with urlopen(request, timeout=30) as response:
        raw = orjson.loads(response.read())
    return catalog_from_models_dev(raw)


def catalog_from_models_dev(raw: Any) -> Catalog:
    if not isinstance(raw, dict):
        raise ValueError("models.dev response must be an object")

    catalog: Catalog = {}
    for provider in ("anthropic", "openai"):
        value = raw.get(provider)
        models = value.get("models") if isinstance(value, dict) else None
        if not isinstance(models, dict):
            raise ValueError(f"models.dev response has no {provider} models")
        catalog[provider] = {}
        for key, model in models.items():
            if not isinstance(model, dict) or (model_price := price_from_model(model)) is None:
                continue
            id = model.get("id")
            catalog[provider][id if isinstance(id, str) else key] = model_price
    return catalog


def price_from_model(model: dict[str, Any]) -> Price | None:
    raw = model.get("cost")
    if not isinstance(raw, dict):
        return None
    input_price = price(raw.get("input"))
    output_price = price(raw.get("output"))
    if input_price is None or output_price is None:
        return None

    result = Price(
        input=input_price,
        output=output_price,
        cache_read=price(raw.get("cache_read")),
        cache_write=price(raw.get("cache_write")),
    )
    tiers = raw.get("tiers")
    if isinstance(tiers, list):
        for tier in tiers:
            rule = tier.get("tier") if isinstance(tier, dict) else None
            if not isinstance(rule, dict) or rule.get("type") != "context":
                continue
            at = rule.get("size")
            if isinstance(at, bool) or not isinstance(at, int) or at < 0:
                continue
            result.long_context_threshold = at
            result.long_context_input = price(tier.get("input"))
            result.long_context_output = price(tier.get("output"))
            result.long_context_cache_read = price(tier.get("cache_read"))
            result.long_context_cache_write = price(tier.get("cache_write"))
            break

    experimental = model.get("experimental")
    modes = experimental.get("modes") if isinstance(experimental, dict) else None
    fast = modes.get("fast") if isinstance(modes, dict) else None
    fast_cost = fast.get("cost") if isinstance(fast, dict) else None
    fast_input = price(fast_cost.get("input")) if isinstance(fast_cost, dict) else None
    if fast_input is not None and input_price:
        result.fast_multiplier = fast_input / input_price
    return result


def price(value: Any) -> float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return float(value)
