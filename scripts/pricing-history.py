# /// script
# requires-python = ">=3.11"
# ///
"""Builds the dated price catalog (src/pricing.tsv) from the git history of models.dev.

Every version of providers/{anthropic,openai}/models/<model>.toml is a price that takes effect on
its commit's date (committer date, UTC): one TSV row per model and `since` date, dollars per
million tokens, an empty cell where a price is not set. An edit within 14 days of the model's
first usable price is a data fix and replaces the earlier price for all dates. A model removed
from models.dev keeps its last price.

    uv run scripts/pricing-history.py [--repo 3rd/models.dev] [--output src/pricing.tsv]
    uv run scripts/pricing-history.py --test
"""

import argparse
import subprocess
import sys
import tomllib
from datetime import UTC, date, datetime, timedelta
from pathlib import Path

URL = "https://github.com/anomalyco/models.dev"
PROVIDERS = ("anthropic", "openai")
CORRECTION = timedelta(days=14)
COLUMNS = (
    "provider",
    "model",
    "since",
    "input",
    "output",
    "cache_read",
    "cache_write",
    "long_context_threshold",
    "long_context_input",
    "long_context_output",
    "long_context_cache_read",
    "long_context_cache_write",
    "fast_multiplier",
)


def price_of(data: dict) -> dict | None:
    """The catalog entry for one model file, or None when it has no usable price."""
    cost = data.get("cost")
    num = lambda x: float(x) if isinstance(x, int | float) and not isinstance(x, bool) else None
    if (
        not isinstance(cost, dict)
        or num(cost.get("input")) is None
        or num(cost.get("output")) is None
    ):
        return None

    price = {"input": num(cost["input"]), "output": num(cost["output"])}
    for key in ("cache_read", "cache_write"):
        if (x := num(cost.get(key))) is not None:
            price[key] = x

    # Long-context prices had three layouts: `[cost.context_over_200k]` until 2026-05-11, then
    # `[[cost.tiers]]` with `context = { min = N }`, since 2026-05-12 `tier = { size = N }`.
    tiers = list(cost.get("tiers") or [])
    if isinstance(cost.get("context_over_200k"), dict):
        tiers.append({**cost["context_over_200k"], "tier": {"size": 200_000}})
    for tier in tiers:
        if not isinstance(tier, dict):
            continue
        rule = tier.get("tier")
        if rule is None and isinstance(tier.get("context"), dict):
            rule = {"size": tier["context"].get("min")}
        if not isinstance(rule, dict) or rule.get("type", "context") != "context":
            continue
        size = rule.get("size")
        if not isinstance(size, int) or isinstance(size, bool) or size < 0:
            continue
        price["long_context_threshold"] = size
        for key in ("input", "output", "cache_read", "cache_write"):
            if (x := num(tier.get(key))) is not None:
                price[f"long_context_{key}"] = x
        break

    # Every fast table so far is the standard price times one factor: keep the factor.
    fast = data.get("experimental", {}).get("modes", {}).get("fast", {}).get("cost")
    fast_input = num(fast.get("input")) if isinstance(fast, dict) else None
    if fast_input is not None and price["input"]:
        price["fast_multiplier"] = fast_input / price["input"]
    return price


def history(changes: list[tuple[date, str, dict | None]]) -> list[dict]:
    """Dated prices of one model from its file versions, oldest first (None: file removed)."""
    periods: list[dict] = []
    first: date | None = None
    for day, _, data in changes:
        price = price_of(data) if data is not None else None
        if price is None:
            continue
        if first is None:
            first = day
        since = max(day, periods[-1]["since_date"]) if periods else day
        if day - first <= CORRECTION:
            periods = [{"since_date": first, **price}]
        elif price != {k: v for k, v in periods[-1].items() if k != "since_date"}:
            periods.append({"since_date": since, **price})
    return [{"since": str(x.pop("since_date")), **x} for x in periods]


def git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args], check=True, capture_output=True, text=True
    ).stdout


def changes_by_model(repo: Path) -> dict[tuple[str, str], list[tuple[date, str, dict | None]]]:
    paths = [f"providers/{x}/models" for x in PROVIDERS]
    log = git(
        repo, "log", "--reverse", "--no-renames", "--format=@%H %cI", "--name-status", "--", *paths
    )
    rows: list[tuple[str, str, str, date]] = []  # status, sha, path, day
    sha, day = "", date.min
    for line in log.splitlines():
        if line.startswith("@"):
            sha, stamp = line[1:].split(" ", 1)
            day = datetime.fromisoformat(stamp).astimezone(UTC).date()
        elif "\t" in line:
            status, path = line.split("\t", 1)
            if len(path.split("/")) == 4 and path.endswith(".toml"):
                rows.append((status, sha, path, day))

    # One `git cat-file` for every version instead of a `git show` each.
    specs = [f"{sha}:{path}" for status, sha, path, _ in rows if status != "D"]
    out = subprocess.run(
        ["git", "-C", str(repo), "cat-file", "--batch"],
        check=True,
        capture_output=True,
        input="\n".join(specs).encode(),
    ).stdout
    blobs: dict[str, bytes] = {}
    pos = 0
    for spec in specs:
        header_end = out.index(b"\n", pos)
        header = out[pos:header_end].split()
        if header[-1] == b"missing":
            pos = header_end + 1
            continue
        size = int(header[2])
        blobs[spec] = out[header_end + 1 : header_end + 1 + size]
        pos = header_end + 1 + size + 1

    result: dict[tuple[str, str], list[tuple[date, str, dict | None]]] = {}
    for status, sha, path, day in rows:
        _, provider, _, name = path.split("/")
        data = None
        if status != "D":
            try:
                data = tomllib.loads(blobs[f"{sha}:{path}"].decode())
            except (KeyError, UnicodeDecodeError, tomllib.TOMLDecodeError):
                continue
        result.setdefault((provider, name.removesuffix(".toml")), []).append((day, sha, data))
    return result


def build(repo: Path) -> dict:
    catalog: dict = {x: {} for x in PROVIDERS}
    for (provider, model), changes in sorted(changes_by_model(repo).items()):
        if periods := history(changes):
            catalog[provider][model] = periods
    for provider in PROVIDERS:
        if not catalog[provider]:
            sys.exit(f"no {provider} prices in {repo}")
    return catalog


def cell(value: object) -> str:
    """A number as the shortest text that reads back to it, `2` rather than `2.0`."""
    if value is None:
        return ""
    text = repr(value)
    return text.removesuffix(".0") if isinstance(value, float) else text


def tsv(catalog: dict) -> str:
    rows = ["\t".join(COLUMNS)]
    for provider, models in sorted(catalog.items()):
        for model, periods in sorted(models.items()):
            for x in sorted(periods, key=lambda x: x["since"]):
                row = {"provider": provider, "model": model, **x}
                rows.append(
                    "\t".join(
                        cell(row.get(k)) if k not in ("provider", "model", "since") else row[k]
                        for k in COLUMNS
                    )
                )
    return "\n".join(rows) + "\n"


def checkout(repo: Path) -> None:
    if (repo / ".git").exists():
        git(repo, "fetch", "--quiet", "origin")
        git(repo, "reset", "--quiet", "--hard", "origin/HEAD")
    else:
        repo.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["git", "clone", "--quiet", URL, str(repo)], check=True)


def test() -> None:
    d = date.fromisoformat
    v = lambda i, o=10: {"cost": {"input": i, "output": o}}

    # A fix within 14 days applies to all dates; a later edit starts a period; removal keeps it.
    got = history(
        [
            (d("2026-01-01"), "a", v(5)),
            (d("2026-01-10"), "b", v(4)),
            (d("2026-03-01"), "c", v(3)),
            (d("2026-04-01"), "d", None),
        ]
    )
    assert got == [
        {"since": "2026-01-01", "input": 4.0, "output": 10.0},
        {"since": "2026-03-01", "input": 3.0, "output": 10.0},
    ], got

    # Metadata-only edits and files without a price add nothing.
    got = history(
        [
            (d("2026-01-01"), "a", {"name": "x"}),
            (d("2026-02-01"), "b", v(2)),
            (d("2026-04-01"), "c", {**v(2), "name": "y"}),
        ]
    )
    assert got == [{"since": "2026-02-01", "input": 2.0, "output": 10.0}], got

    # Tiers without a type are context tiers; a fast table becomes a multiplier.
    data = {
        "cost": {
            "input": 2,
            "output": 8,
            "cache_read": 0.2,
            "tiers": [
                {"tier": {"type": "other", "size": 1}, "input": 9},
                {"tier": {"size": 272_000}, "input": 4, "output": 12},
            ],
        },
        "experimental": {"modes": {"fast": {"cost": {"input": 4, "output": 16}}}},
    }
    assert price_of(data) == {
        "input": 2.0,
        "output": 8.0,
        "cache_read": 0.2,
        "long_context_threshold": 272_000,
        "long_context_input": 4.0,
        "long_context_output": 12.0,
        "fast_multiplier": 2.0,
    }
    # The two older long-context layouts.
    long = {"long_context_input": 5.0, "long_context_output": 22.5}
    old = {"cost": {"input": 2.5, "output": 15, "context_over_200k": {"input": 5, "output": 22.5}}}
    assert price_of(old) == {
        "input": 2.5,
        "output": 15.0,
        "long_context_threshold": 200_000,
        **long,
    }
    mid = {
        "cost": {
            "input": 2.5,
            "output": 15,
            "tiers": [{"context": {"min": 272_000}, "input": 5, "output": 22.5}],
        }
    }
    assert price_of(mid) == {
        "input": 2.5,
        "output": 15.0,
        "long_context_threshold": 272_000,
        **long,
    }
    assert price_of({"cost": {"output": 1}}) is None

    # The TSV: a header, rows sorted by provider, model and date, empty cells for missing prices.
    catalog = {
        "openai": {"b": [{"since": "2026-01-01", "input": 1.25, "output": 10.0}]},
        "anthropic": {
            "a": [
                {"since": "2026-02-01", "input": 3.0, "output": 15.0, "fast_multiplier": 1.75},
                {
                    "since": "2026-01-01",
                    "input": 5.0,
                    "output": 25.0,
                    "long_context_threshold": 200_000,
                },
            ][::-1]
        },
    }
    lines = tsv(catalog).splitlines()
    assert lines[0].split("\t") == list(COLUMNS)
    assert lines[1:] == [
        "anthropic\ta\t2026-01-01\t5\t25\t\t\t200000\t\t\t\t\t",
        "anthropic\ta\t2026-02-01\t3\t15\t\t\t\t\t\t\t\t1.75",
        "openai\tb\t2026-01-01\t1.25\t10\t\t\t\t\t\t\t\t",
    ], lines
    assert price_of({"cost": {"input": True, "output": 1}}) is None
    print("ok")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, default=Path("3rd/models.dev"))
    parser.add_argument("--output", type=Path, default=Path("src/pricing.tsv"))
    parser.add_argument("--test", action="store_true", help="check the rules and exit")
    args = parser.parse_args()
    if args.test:
        return test()

    checkout(args.repo)
    catalog = build(args.repo)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(tsv(catalog))
    count = sum(len(x) for x in catalog.values())
    print(f"{args.output}: {count} models")


if __name__ == "__main__":
    main()
