import argparse
import sys
from datetime import date
from importlib.metadata import version
from pathlib import Path
from typing import Any

import orjson

from . import metrics, output, pricing
from .config import Config, StatsBy
from .metrics import DateRange, Granularity, Period, UsageSummary
from .model import Provider
from .parser import parse_history


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(
        prog="cctrail",
        description="Report token usage and active time from local Claude Code and Codex history.",
    )
    root.add_argument("--version", action="version", version=f"%(prog)s {version('cctrail')}")
    add_no_config(root)
    commands = root.add_subparsers(dest="command", required=True, metavar="{usage,stats}")

    usage = commands.add_parser("usage", help="Show token usage over time.")
    add_no_config(usage)
    add_report_args(usage)
    usage.add_argument(
        "--by",
        dest="granularity",
        choices=list(Granularity),
        type=Granularity,
        default=Granularity.DAY,
        help="Calendar grouping.",
    )
    usage.add_argument("--models", action="store_true", help="Break down each period by model.")

    stats = commands.add_parser(
        "stats",
        help="Show statistics by project, group, or working directory.",
    )
    add_no_config(stats)
    add_report_args(stats)
    stats.add_argument(
        "--by",
        choices=list(StatsBy),
        type=StatsBy,
        default=StatsBy.PROJECT,
        help="Group statistics.",
    )

    update = commands.add_parser("update-pricing")
    add_no_config(update)
    return root


def add_no_config(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--no-config",
        action="store_true",
        default=argparse.SUPPRESS,
        help="Run without loading project configuration.",
    )


def add_report_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("period", nargs="?", choices=list(Period), type=Period)
    parser.add_argument("--since", type=parse_date, metavar="YYYY-MM-DD")
    parser.add_argument("--until", type=parse_date, metavar="YYYY-MM-DD")
    parser.add_argument("--provider", choices=list(Provider), type=Provider)
    parser.add_argument("--json", action="store_true", help="Output machine-readable JSON.")


def main(argv: list[str] | None = None) -> int:
    root = parser()
    args = root.parse_args(argv)
    if getattr(args, "period", None) and (args.since or args.until):
        root.error("a period cannot be combined with --since or --until")
    try:
        if args.command == "usage":
            run_usage(args)
        elif args.command == "stats":
            run_stats(args)
        else:
            update_pricing()
    except (OSError, ValueError) as e:
        print(e, file=sys.stderr)
        return 2
    return 0


def run_usage(args: argparse.Namespace) -> None:
    date_range = DateRange.resolve(args.period, args.since, args.until)
    sessions = parse_history(args.provider)
    granularity = args.granularity
    buckets = metrics.usage_by_period(sessions, granularity, date_range)
    total = UsageSummary()
    for summary in buckets.values():
        total.usage += summary.usage
        total.cost += summary.cost
        total.active_time += summary.active_time

    if args.json:
        periods = []
        for bucket, summary in buckets.items():
            start, end = granularity.bounds(bucket, date_range)
            periods.append(
                {
                    "start": str(start),
                    "end": str(end),
                    "usage": output.usage_json(summary.usage),
                    "cost_usd": output.cost_json(summary.cost),
                    "active_seconds": int(summary.active_time.total_seconds()),
                    "models": [output.model_json(x) for x in summary.models] if args.models else [],
                }
            )
        print_json(
            {
                "schema_version": 1,
                "command": "usage",
                "query": {
                    "period": args.period.value if args.period else None,
                    "since": str(date_range.since) if date_range.since else None,
                    "until": str(date_range.until) if date_range.until else None,
                    "provider": args.provider.value if args.provider else None,
                    "granularity": granularity.value,
                    "models": args.models,
                },
                "periods": periods,
                "total": {
                    "usage": output.usage_json(total.usage),
                    "cost_usd": output.cost_json(total.cost),
                    "active_seconds": int(total.active_time.total_seconds()),
                },
            }
        )
        return

    rows = []
    for bucket, summary in buckets.items():
        rows.append(
            output.usage_row(
                granularity.label(bucket, date_range),
                summary.usage,
                summary.cost,
                summary.active_time,
            )
        )
        if args.models:
            for model in summary.models:
                suffix = " (fast)" if model.fast else ""
                rows.append(
                    output.usage_row(
                        f"  {model.provider.value}/{model.model}{suffix}",
                        model.usage,
                        model.cost,
                        None,
                    )
                )
    rows.append(output.usage_row("total", total.usage, total.cost, total.active_time))
    label = {
        Granularity.DAY: "Date",
        Granularity.WEEK: "Week",
        Granularity.MONTH: "Month",
    }[granularity]
    output.usage_table(f"{label} / Model" if args.models else label, rows)


def run_stats(args: argparse.Namespace) -> None:
    date_range = DateRange.resolve(args.period, args.since, args.until)
    by = StatsBy.WORKDIR if getattr(args, "no_config", False) else args.by
    config = Config() if by is StatsBy.WORKDIR else Config.load()
    sessions = parse_history(args.provider)
    stats = metrics.stats_by(sessions, date_range, config, by)
    total = metrics.combine_stats(stats)

    if args.json:
        print_json(
            {
                "schema_version": 1,
                "command": "stats",
                "query": {
                    "period": args.period.value if args.period else None,
                    "since": str(date_range.since) if date_range.since else None,
                    "until": str(date_range.until) if date_range.until else None,
                    "provider": args.provider.value if args.provider else None,
                    "by": by.value,
                },
                "groups": [output.stats_json(x, by) for x in stats],
                "total": output.stats_total_json(total, len(stats), by),
            }
        )
        return

    rows = output.stats_rows(stats, by)
    rows.append(output.stats_row(total, by))
    output.stats_table(by, rows)


def update_pricing() -> None:
    path = Path(__file__).with_name("pricing.json")
    pricing.update_catalog(path)
    print(f"Updated {path}")


def print_json(value: Any) -> None:
    print(orjson.dumps(value).decode())


def parse_date(value: str) -> date:
    try:
        return date.fromisoformat(value)
    except ValueError as e:
        raise argparse.ArgumentTypeError(str(e)) from e


if __name__ == "__main__":
    raise SystemExit(main())
