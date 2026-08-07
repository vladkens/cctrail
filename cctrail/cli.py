import argparse
import calendar
from collections import Counter
from datetime import date, datetime, timedelta
from importlib.metadata import PackageNotFoundError, version

from rich.console import Console
from rich.measure import Measurement
from rich.table import Table
from rich.text import Text

from .history import iter_sessions
from .metrics import ModelSummary, by_project, usage_by_period, usage_messages
from .models import Usage
from .pricing import get_pricing
from .week import first_weekday

PERIODS = (
    "today",
    "yesterday",
    "this-week",
    "last-week",
    "this-month",
    "last-month",
    "all",
)


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


def duration(value: timedelta) -> str:
    minutes = int((value.total_seconds() + 30) // 60)
    hours, minutes = divmod(minutes, 60)
    return f"{hours}h {minutes:02d}m"


def format_local_date(value: datetime) -> str:
    if value.tzinfo is not None:
        value = value.astimezone()

    return value.date().isoformat()


def week_start(value: date, first_weekday: int) -> date:
    return value - timedelta(days=(value.weekday() - first_weekday) % 7)


def month_start(value: date) -> date:
    return value.replace(day=1)


def next_month(value: date) -> date:
    return (value.replace(day=28) + timedelta(days=4)).replace(day=1)


def period_range(
    period: str | None,
    *,
    today: date | None = None,
    first_weekday: int = calendar.MONDAY,
) -> tuple[date | None, date | None]:
    today = today or date.today()
    if period in (None, "all"):
        return None, None
    if period == "today":
        return today, today
    if period == "yesterday":
        yesterday = today - timedelta(days=1)
        return yesterday, yesterday
    if period == "this-week":
        start = week_start(today, first_weekday)
        return start, start + timedelta(days=6)
    if period == "last-week":
        end = week_start(today, first_weekday) - timedelta(days=1)
        return end - timedelta(days=6), end
    if period == "this-month":
        start = month_start(today)
        return start, next_month(start) - timedelta(days=1)
    if period == "last-month":
        end = month_start(today) - timedelta(days=1)
        return month_start(end), end

    raise ValueError(f"Unsupported period: {period}")


def date_range(args: argparse.Namespace) -> tuple[date | None, date | None]:
    if args.period is None:
        return args.since, args.until
    return period_range(args.period, first_weekday=first_weekday())


def period_label(
    value: date,
    granularity: str,
    *,
    since: date | None = None,
    until: date | None = None,
) -> str:
    if granularity == "day":
        return value.isoformat()
    if granularity == "week":
        end = value + timedelta(days=6)
    else:
        end = next_month(value) - timedelta(days=1)

    start = max(value, since) if since is not None else value
    end = min(end, until) if until is not None else end
    if start == value and granularity == "month" and end == next_month(value) - timedelta(days=1):
        return value.strftime("%Y-%m")
    if start == end:
        return start.isoformat()
    return f"{start.isoformat()} – {end.strftime('%m-%d')}"


def model_label(summary: ModelSummary) -> Text:
    suffix = " (fast)" if summary.speed == "fast" else ""
    return Text(f"  {summary.provider}/{summary.model}{suffix}", style="default")


def add_usage_columns(table: Table, *, full: bool, models: bool, granularity: str) -> None:
    label = {"day": "Date", "week": "Week", "month": "Month"}[granularity]
    if models:
        label += " / Model"

    columns = [(label, "cyan"), ("Input", None), ("Output", None)]
    if full:
        columns += [("Reasoning", None), ("Cache W", None), ("Cache R", None)]
    columns += [("Total", None), ("Cost", "green"), ("Time", "yellow")]
    for column, style in columns:
        table.add_column(
            column,
            justify="right" if column != label else "left",
            style=style,
            no_wrap=True,
        )


def usage_row(
    label: str | Text,
    usage: Usage,
    cost: float,
    active_time: timedelta | None,
    *,
    full: bool,
) -> tuple[str | Text, ...]:
    values: list[str | Text] = [label, human(usage.input), human(usage.output)]
    if full:
        values += [human(usage.reasoning), human(usage.cache_write), human(usage.cache_read)]
    values += [
        human(usage.total),
        money(cost),
        duration(active_time) if active_time is not None else "",
    ]
    return tuple(values)


def print_table(table: Table) -> None:
    console = Console()
    required_width = Measurement.get(console, console.options.update_width(1000), table).maximum
    console.width = max(console.width, required_width)
    console.print(table)


def cmd_usage(args: argparse.Namespace) -> None:
    since, until = date_range(args)
    buckets = usage_by_period(
        iter_sessions(),
        granularity=args.granularity,
        since=since,
        until=until,
        first_weekday=first_weekday(),
    )

    table = Table(box=None, header_style="bold", pad_edge=False, collapse_padding=True)
    add_usage_columns(
        table,
        full=args.full,
        models=args.models,
        granularity=args.granularity,
    )

    total = Usage()
    total_cost = 0.0
    total_active = timedelta()
    for period in sorted(buckets):
        summary = buckets[period]
        usage = summary.usage
        total = total + usage
        total_cost += summary.cost
        total_active += summary.active_time
        table.add_row(
            *usage_row(
                period_label(
                    period,
                    args.granularity,
                    since=since,
                    until=until,
                ),
                usage,
                summary.cost,
                summary.active_time,
                full=args.full,
            )
        )
        if args.models:
            for model in summary.models:
                table.add_row(
                    *usage_row(
                        model_label(model),
                        model.usage,
                        model.cost,
                        None,
                        full=args.full,
                    )
                )

    table.add_section()
    table.add_row(
        *usage_row("total", total, total_cost, total_active, full=args.full),
        style="bold",
    )
    print_table(table)


def cmd_projects(args: argparse.Namespace) -> None:
    since, until = date_range(args)
    projects = sorted(
        by_project(iter_sessions(), since=since, until=until).values(),
        key=lambda p: p.usage.total,
        reverse=True,
    )

    table = Table(box=None, header_style="bold", pad_edge=False, collapse_padding=True)
    table.add_column("Project", style="cyan")
    table.add_column("Sessions", justify="right")
    table.add_column("Total tokens", justify="right")
    table.add_column("Last active", style="dim")
    table.add_column("Cost", justify="right", style="green")
    table.add_column("Time", justify="right", style="yellow")
    for project in projects:
        table.add_row(
            project.project,
            str(project.session_count),
            human(project.usage.total),
            format_local_date(project.last_activity) if project.last_activity else "-",
            money(project.cost),
            duration(project.active_time),
        )

    if args.full:
        print_table(table)
    else:
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


def add_date_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "period",
        nargs="?",
        choices=PERIODS,
        metavar="PERIOD",
        help="today, yesterday, this-week, last-week, this-month, last-month, or all",
    )
    parser.add_argument(
        "--since",
        type=parse_date,
        default=None,
        metavar="YYYY-MM-DD",
        help="start of a custom date range",
    )
    parser.add_argument(
        "--until",
        type=parse_date,
        default=None,
        metavar="YYYY-MM-DD",
        help="end of a custom date range",
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="cctrail")
    subparsers = parser.add_subparsers(dest="command", required=True)

    usage_parser = subparsers.add_parser("usage", help="Token usage over time")
    add_date_arguments(usage_parser)
    usage_parser.add_argument(
        "--by",
        choices=("day", "week", "month"),
        default="day",
        dest="granularity",
        help="calendar grouping (default: day)",
    )
    usage_parser.add_argument(
        "--models", action="store_true", help="Break down each period by model"
    )
    usage_parser.add_argument("--full", action="store_true", help="Show detailed token columns")
    usage_parser.set_defaults(func=cmd_usage)

    projects_parser = subparsers.add_parser("projects", help="Token usage aggregated by project")
    add_date_arguments(projects_parser)
    projects_parser.add_argument("--full", action="store_true", help="Show full project paths")
    projects_parser.set_defaults(func=cmd_projects)

    audit_parser = subparsers.add_parser("audit", help="Check pricing coverage")
    audit_parser.set_defaults(func=cmd_audit)

    args = parser.parse_args(argv)
    if getattr(args, "period", None) is not None and (
        args.since is not None or args.until is not None
    ):
        parser.error("PERIOD cannot be combined with --since or --until")
    if getattr(args, "since", None) and getattr(args, "until", None) and args.since > args.until:
        parser.error("--since must not be later than --until")

    try:
        args.func(args)
    except KeyboardInterrupt:
        return 130

    return 0
