import argparse
from datetime import date, datetime

from rich.console import Console
from rich.table import Table

from .claude import iter_sessions
from .metrics import by_project, usage_by_day
from .models import Usage


def parse_date(value: str) -> date:
    return datetime.strptime(value, "%Y-%m-%d").date()


def human(n: int) -> str:
    if n >= 1_000_000:
        return f"{n / 1_000_000:.2f}M"

    if n >= 1_000:
        return f"{n / 1_000:.1f}K"

    return str(n)


def cmd_usage(args: argparse.Namespace) -> None:
    buckets = usage_by_day(iter_sessions(args.root))
    days = sorted(
        day
        for day in buckets
        if (args.since is None or day >= args.since) and (args.until is None or day <= args.until)
    )

    table = Table(box=None, header_style="bold")
    for column in ("Date", "Input", "Output", "Cache write", "Cache read", "Total"):
        table.add_column(column, justify="right" if column != "Date" else "left")

    total = Usage()
    for day in days:
        usage = buckets[day]
        total = total + usage
        table.add_row(
            day.isoformat(),
            human(usage.input),
            human(usage.output),
            human(usage.cache_write),
            human(usage.cache_read),
            human(usage.total),
        )

    table.add_section()
    table.add_row(
        "total",
        human(total.input),
        human(total.output),
        human(total.cache_write),
        human(total.cache_read),
        human(total.total),
        style="bold",
    )
    Console().print(table)


def cmd_projects(args: argparse.Namespace) -> None:
    projects = sorted(
        by_project(iter_sessions(args.root)).values(),
        key=lambda p: p.usage.total,
        reverse=True,
    )

    table = Table(box=None, header_style="bold")
    table.add_column("Project")
    table.add_column("Sessions", justify="right")
    table.add_column("Total tokens", justify="right")
    table.add_column("Last active")
    for project in projects:
        table.add_row(
            project.project,
            str(project.session_count),
            human(project.usage.total),
            project.last_activity.date().isoformat() if project.last_activity else "-",
        )

    Console().print(table)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="cctrail")
    subparsers = parser.add_subparsers(dest="command", required=True)

    usage_parser = subparsers.add_parser("usage", help="Token usage by day")
    usage_parser.add_argument(
        "--root", default=None, help="Claude projects dir (default: ~/.claude/projects)"
    )
    usage_parser.add_argument("--since", type=parse_date, default=None, metavar="YYYY-MM-DD")
    usage_parser.add_argument("--until", type=parse_date, default=None, metavar="YYYY-MM-DD")
    usage_parser.set_defaults(func=cmd_usage)

    projects_parser = subparsers.add_parser("projects", help="Token usage aggregated by project")
    projects_parser.add_argument(
        "--root", default=None, help="Claude projects dir (default: ~/.claude/projects)"
    )
    projects_parser.set_defaults(func=cmd_projects)

    args = parser.parse_args(argv)
    args.func(args)
    return 0
