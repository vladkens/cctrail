import argparse
from collections import Counter
from datetime import date, datetime
from importlib.metadata import PackageNotFoundError, version

from rich.console import Console
from rich.table import Table

from .history import iter_sessions
from .metrics import by_project, usage_by_day, usage_messages
from .models import Usage
from .pricing import get_pricing


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
    buckets = usage_by_day(iter_sessions())
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
        by_project(iter_sessions()).values(),
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


def cmd_audit(args: argparse.Namespace) -> None:
    sessions = list(iter_sessions())
    messages = usage_messages(sessions)
    issue_messages: Counter[tuple[str, str, str]] = Counter()
    issue_tokens: Counter[tuple[str, str, str]] = Counter()
    total_tokens = 0

    for session, message in messages:
        assert message.usage is not None
        total_tokens += message.usage.total
        if message.model is None:
            issue = "missing model", session.provider, "-"
        elif get_pricing(message.model, provider=session.provider) is None:
            issue = "missing pricing", session.provider, message.model
        else:
            continue

        issue_messages[issue] += 1
        issue_tokens[issue] += message.usage.total

    unpriced_messages = sum(issue_messages.values())
    unpriced_tokens = sum(issue_tokens.values())
    try:
        current_version = version("cctrail")
    except PackageNotFoundError:
        current_version = "unknown"

    summary = Table(title="Pricing audit", box=None, header_style="bold")
    summary.add_column("Metric")
    summary.add_column("Value", justify="right")
    summary.add_row("cctrail version", current_version)
    summary.add_row("Sessions", f"{len(sessions):,}")
    summary.add_row("Billable messages", f"{len(messages):,}")
    summary.add_row("Billable tokens", f"{total_tokens:,}")
    summary.add_row("Unpriced messages", f"{unpriced_messages:,}")
    summary.add_row("Unpriced tokens", f"{unpriced_tokens:,}")

    console = Console()
    console.print(summary)
    if not issue_messages:
        console.print("No pricing issues found.")
        return

    issues = Table(title="Pricing issues", box=None, header_style="bold")
    issues.add_column("Issue")
    issues.add_column("Provider")
    issues.add_column("Model")
    issues.add_column("Messages", justify="right")
    issues.add_column("Tokens", justify="right")
    for issue in sorted(issue_messages):
        kind, provider, model = issue
        issues.add_row(
            kind,
            provider,
            model,
            f"{issue_messages[issue]:,}",
            f"{issue_tokens[issue]:,}",
        )
    console.print(issues)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="cctrail")
    subparsers = parser.add_subparsers(dest="command", required=True)

    usage_parser = subparsers.add_parser("usage", help="Token usage by day")
    usage_parser.add_argument("--since", type=parse_date, default=None, metavar="YYYY-MM-DD")
    usage_parser.add_argument("--until", type=parse_date, default=None, metavar="YYYY-MM-DD")
    usage_parser.set_defaults(func=cmd_usage)

    projects_parser = subparsers.add_parser("projects", help="Token usage aggregated by project")
    projects_parser.set_defaults(func=cmd_projects)

    audit_parser = subparsers.add_parser("audit", help="Check pricing coverage")
    audit_parser.set_defaults(func=cmd_audit)

    args = parser.parse_args(argv)
    try:
        args.func(args)
    except KeyboardInterrupt:
        return 130

    return 0
