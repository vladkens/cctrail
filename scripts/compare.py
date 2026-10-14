# /// script
# requires-python = ">=3.11"
# ///
"""Compares the branch's binary with a reference build (usually `main`) on the local history.

Both read the same price catalog (src/pricing.tsv through XDG_CACHE_HOME, no refresh) and stop at
yesterday, so sessions running now don't change the numbers between the two runs. Totals must
match: tokens and sessions exactly, costs within 1e-9, active time within a second per group
(rounding when time is divided between groups). Group and period differences are listed, not
failed on: grouping may change by design.

    uv run scripts/compare.py --ref ../main/target/release/cctrail [--bin target/release/cctrail]
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from datetime import date, timedelta
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def cases(until: str, since: str) -> list[list[str]]:
    window = ["--until", until]
    week = ["--since", since, "--until", until]
    return [
        ["--by", "dir", *window],
        ["--by", "dir", *week],
        [*window],
        ["--by", "group", *window],
        ["days", "--models", *window],
        ["weeks", "--models", *window],
        ["months", "--models", *week],
    ]


def run(cmd: list[str], env: dict[str, str]) -> dict:
    result = subprocess.run(cmd, env=env, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        sys.exit(f"{' '.join(cmd)} failed:\n{result.stderr}")
    return json.loads(result.stdout)


def total_problems(ref: dict, out: dict, slack: int) -> list[str]:
    a, b = ref["total"], out["total"]
    usage = a["usage"]
    problems = [f"usage.{k}: {usage.get(k)} != {v}" for k, v in b["usage"].items() if usage.get(k) != v]
    if abs(a["cost_usd"] - b["cost_usd"]) > 1e-9:
        problems.append(f"cost_usd: {a['cost_usd']} != {b['cost_usd']}")
    if abs(a["active_seconds"] - b["active_seconds"]) > slack:
        problems.append(f"active_seconds: {a['active_seconds']} != {b['active_seconds']}")
    for key in ["sessions", "first_activity", "last_activity"]:
        if a.get(key) != b.get(key):
            problems.append(f"{key}: {a.get(key)} != {b.get(key)}")
    return problems


def rows(value: dict) -> dict[str, dict]:
    if "periods" in value:
        return {x["start"]: x for x in value["periods"]}
    key = lambda x: x.get("workdir") or "/".join(filter(None, [x.get("group"), x.get("project")]))
    return {key(x): x for x in value["groups"]}


def changes(ref: dict, out: dict) -> list[str]:
    a, b = rows(ref), rows(out)
    moves = []
    for name in set(a) | set(b):
        x, y = a.get(name), b.get(name)
        dt = (y["active_seconds"] if y else 0) - (x["active_seconds"] if x else 0)
        dk = (y["usage"]["total"] if y else 0) - (x["usage"]["total"] if x else 0)
        dc = (y["cost_usd"] if y else 0) - (x["cost_usd"] if x else 0)
        # Any field may move (cost, sessions, token kinds, models), not only time and totals.
        if x != y:
            moves.append((abs(dt), name, dt, dk, dc))
    moves.sort(key=lambda m: (-m[0], m[1]))
    return [f"{n}: {dt / 3600:+.1f}h, {dk:+,} tokens, ${dc:+.2f}" for _, n, dt, dk, dc in moves]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--ref", default=os.environ.get("REF"), help="the reference binary")
    parser.add_argument("--bin", default=str(ROOT / "target/release/cctrail"))
    args = parser.parse_args()
    if not args.ref:
        sys.exit("pass --ref (or REF=) a build of main, e.g. from `git worktree add ../main main`")

    yesterday = date.today() - timedelta(days=1)
    until, since = str(yesterday), str(yesterday - timedelta(days=6))
    with tempfile.TemporaryDirectory() as cache:
        (Path(cache) / "cctrail").mkdir()
        shutil.copy(ROOT / "src/pricing.tsv", Path(cache) / "cctrail/pricing.tsv")
        env = os.environ | {"XDG_CACHE_HOME": cache, "CCTRAIL_NO_UPDATE": "1"}

        failed = 0
        for case in cases(until, since):
            ref = run([args.ref, *case, "--json"], env)
            out = run([args.bin, *case, "--json"], env)
            groups = max(len(rows(ref)), len(rows(out)))
            problems = total_problems(ref, out, groups if "groups" in ref else 0)
            failed += bool(problems)
            moved = changes(ref, out)
            status = "FAIL" if problems else "ok  "
            print(f"{status} cctrail {' '.join(case)}: {len(rows(ref))} → {len(rows(out))} rows")
            for line in problems + moved[:10]:
                print(f"     {line}")
            if len(moved) > 10:
                print(f"     ... {len(moved) - 10} more")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
