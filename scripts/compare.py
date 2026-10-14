# /// script
# requires-python = ">=3.11"
# ///
"""Compares the Rust binary with the Python cctrail 0.2.1 on the local history.

Both read the same price catalog (src/pricing.json through XDG_CACHE_HOME) and stop at yesterday,
so sessions running now don't change the numbers between the two runs. Costs may differ by 1e-9;
everything else must match exactly.

    uv run scripts/compare.py [--ref "uvx cctrail==0.2.1"] [--bin target/release/cctrail]
"""

import argparse
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
from datetime import date, timedelta
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
IGNORED = {"schema_version", "command", "query", "unpriced_models"}


def cases(until: str, since: str) -> list[tuple[list[str], list[str]]]:
    window = ["--until", until]
    week = ["--since", since, "--until", until]
    return [
        (["stats", "--by", "workdir", *window], ["--by", "dir", *window]),
        (["stats", "--by", "workdir", *week], ["--by", "dir", *week]),
        (["stats", *window], [*window]),
        (["stats", "--by", "group", *window], ["--by", "group", *window]),
        (["usage", "--models", *window], ["days", "--models", *window]),
        (["usage", "--by", "week", "--models", *window], ["weeks", "--models", *window]),
        (["usage", "--by", "month", "--models", *week], ["months", "--models", *week]),
    ]


def run(cmd: list[str], env: dict[str, str]) -> dict:
    result = subprocess.run(cmd, env=env, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        sys.exit(f"{shlex.join(cmd)} failed:\n{result.stderr}")
    return json.loads(result.stdout)


def diff(a, b, path: str, out: list[str]) -> None:
    if path.endswith(".cost_usd") and isinstance(a, float | int) and isinstance(b, float | int):
        if abs(a - b) > 1e-9:
            out.append(f"{path}: {a} != {b}")
    elif isinstance(a, dict) and isinstance(b, dict):
        for key in sorted(set(a) | set(b)):
            if path == "" and key in IGNORED:
                continue
            if key not in a or key not in b:
                out.append(f"{path}.{key}: only in {'ref' if key in a else 'bin'}")
            else:
                diff(a[key], b[key], f"{path}.{key}", out)
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            out.append(f"{path}: {len(a)} != {len(b)} items")
        for i, (x, y) in enumerate(zip(a, b, strict=False)):
            diff(x, y, f"{path}[{i}]", out)
    elif a != b:
        out.append(f"{path}: {a!r} != {b!r}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--ref", default="uvx cctrail==0.2.1")
    parser.add_argument("--bin", default=str(ROOT / "target/release/cctrail"))
    args = parser.parse_args()

    yesterday = date.today() - timedelta(days=1)
    until, since = str(yesterday), str(yesterday - timedelta(days=6))
    with tempfile.TemporaryDirectory() as cache:
        (Path(cache) / "cctrail").mkdir()
        shutil.copy(ROOT / "src/pricing.json", Path(cache) / "cctrail/pricing.json")
        env = os.environ | {"XDG_CACHE_HOME": cache}

        failed = 0
        for ref_args, bin_args in cases(until, since):
            ref = run([*shlex.split(args.ref), *ref_args, "--json"], env)
            out = run([args.bin, *bin_args, "--json"], env)
            problems: list[str] = []
            diff(ref, out, "", problems)
            failed += bool(problems)
            print(f"{'FAIL' if problems else 'ok  '} cctrail {shlex.join(bin_args)}")
            for line in problems[:20]:
                print(f"     {line}")
            if len(problems) > 20:
                print(f"     ... {len(problems) - 20} more")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
