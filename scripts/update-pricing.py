#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["cctrail"]
#
# [tool.uv.sources]
# cctrail = { path = "..", editable = true }
# ///
import json
import urllib.request
from pathlib import Path
from typing import Any

from cctrail.pricing import catalog_from_models_dev

URL = "https://models.dev/api.json"
PRICING_PATH = Path(__file__).parents[1] / "cctrail" / "pricing.json"


def load_catalog() -> dict[str, Any]:
    try:
        value = json.loads(PRICING_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    return value if isinstance(value, dict) else {}


def flatten(catalog: dict[str, Any]) -> dict[str, Any]:
    return {
        f"{provider}/{model}": pricing
        for provider, models in catalog.items()
        if isinstance(models, dict)
        for model, pricing in models.items()
    }


def format_value(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def catalog_changes(before: dict[str, Any], after: dict[str, Any]) -> list[str]:
    before = flatten(before)
    after = flatten(after)
    changes = []
    for model in sorted(before.keys() | after.keys()):
        old = before.get(model)
        new = after.get(model)
        if old == new:
            continue
        kind = "added" if old is None else "removed" if new is None else "changed"
        changes.append(f"{kind} {model}: {format_value(old)} → {format_value(new)}")
    return changes


def main() -> int:
    request = urllib.request.Request(URL, headers={"User-Agent": "cctrail"})
    with urllib.request.urlopen(request, timeout=30) as response:
        catalog = catalog_from_models_dev(json.load(response))

    changes = catalog_changes(load_catalog(), catalog)
    if not changes:
        print("Nothing changed")
        return 0

    print("\n".join(changes))
    PRICING_PATH.write_text(json.dumps(catalog, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
