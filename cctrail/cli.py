import argparse
from datetime import date, datetime

from rich.console import Console
from rich.table import Table

from .history import iter_sessions
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


def money(n: float) -> str:
    return f"${n:.2f}"


def format_local_date(value: datetime) -> str:
    if value.tzinfo is not None:
        value = value.astimezone()

    return value.date().isoformat()


def cmd_usage(args: argparse.Namespace) -> None:
    buckets = usage_by_day(iter_sessions(args.root))
    days = sorted(
        day
        for day in buckets
        if (args.since is None or day >= args.since) and (args.until is None or day <= args.until)
    )

    table = Table(box=None, header_style="bold")
    for column in (
        "Date",
        "Input",
        "Output",
        "Reasoning",
        "Cache write",
        "Cache read",
        "Total",
        "Cost",
    ):
        table.add_column(column, justify="right" if column != "Date" else "left")

    total = Usage()
    total_cost = 0.0
    for day in days:
        summary = buckets[day]
        usage = summary.usage
        total = total + usage
        total_cost += summary.cost
        table.add_row(
            day.isoformat(),
            human(usage.input),
            human(usage.output),
            human(usage.reasoning),
            human(usage.cache_write),
            human(usage.cache_read),
            human(usage.total),
            money(summary.cost),
        )

    table.add_section()
    table.add_row(
        "total",
        human(total.input),
        human(total.output),
        human(total.reasoning),
        human(total.cache_write),
        human(total.cache_read),
        human(total.total),
        money(total_cost),
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
    table.add_column("Cost", justify="right")
    table.add_column("Last active")
    for project in projects:
        table.add_row(
            project.project,
            str(project.session_count),
            human(project.usage.total),
            money(project.cost),
            format_local_date(project.last_activity) if project.last_activity else "-",
        )

    Console().print(table)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="cctrail")
    subparsers = parser.add_subparsers(dest="command", required=True)

    usage_parser = subparsers.add_parser("usage", help="Token usage by day")
    usage_parser.add_argument(
        "--root", default=None, help="History dir (default: Claude and Codex homes)"
    )
    usage_parser.add_argument("--since", type=parse_date, default=None, metavar="YYYY-MM-DD")
    usage_parser.add_argument("--until", type=parse_date, default=None, metavar="YYYY-MM-DD")
    usage_parser.set_defaults(func=cmd_usage)

    projects_parser = subparsers.add_parser("projects", help="Token usage aggregated by project")
    projects_parser.add_argument(
        "--root", default=None, help="History dir (default: Claude and Codex homes)"
    )
    projects_parser.set_defaults(func=cmd_projects)

    args = parser.parse_args(argv)
    try:
        args.func(args)
    except KeyboardInterrupt:
        return 130

    return 0
