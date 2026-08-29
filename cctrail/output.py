import math
import os
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

from rich.console import Console
from rich.table import Table
from rich.text import Text

from .config import StatsBy, split_project
from .metrics import ModelSummary, Stat, combine_stats
from .model import Usage


def usage_table(label: str, rows: list[list[str]]) -> None:
    headers = [label, "Input", "Output", "Total", "Cost", "Time"]
    print_table(headers, rows)


def stats_table(by: StatsBy, rows: list[list[str]]) -> None:
    label = {
        StatsBy.PROJECT: "Project",
        StatsBy.GROUP: "Group",
        StatsBy.WORKDIR: "Workdir",
    }[by]
    headers = [label, "Sessions", "Total tokens", "Last active", "Cost", "Time"]
    print_table(headers, rows, dim_column=3, max_first_column=60)


def print_table(
    headers: list[str],
    rows: list[list[str]],
    dim_column: int | None = None,
    max_first_column: int | None = None,
) -> None:
    table = Table(box=None, header_style="bold", pad_edge=False, collapse_padding=True)
    for i, header in enumerate(headers):
        table.add_column(
            header,
            justify="right" if i > 0 and header != "Last active" else "left",
            style="dim" if i == dim_column else None,
            max_width=max_first_column if i == 0 else None,
            overflow="ellipsis" if i == 0 else "fold",
            no_wrap=True,
        )

    for row in rows:
        bold = row[0] in {"total", "@unmapped"}
        values: list[str | Text] = []
        for i, value in enumerate(row):
            style = None
            if i == 0 and not value.startswith("  "):
                style = "cyan"
            elif i == 4:
                style = "green"
            elif i == 5:
                style = "yellow"
            values.append(Text(value, style=style) if style else value)
        table.add_row(*values, style="bold" if bold else None)

    Console(highlight=False).print(table)


def usage_json(usage: Usage) -> dict[str, int]:
    return {
        "input": usage.input,
        "cache_write_5m": usage.cache_write_5m,
        "cache_write_1h": usage.cache_write_1h,
        "cache_write": usage.cache_write,
        "cache_read": usage.cache_read,
        "output": usage.output,
        "reasoning": usage.reasoning,
        "total": usage.total,
    }


def model_json(model: ModelSummary) -> dict[str, Any]:
    return {
        "provider": model.provider.value,
        "model": model.model,
        "speed": "fast" if model.fast else None,
        "usage": usage_json(model.usage),
        "cost_usd": cost_json(model.cost),
    }


def stats_json(stat: Stat, by: StatsBy) -> dict[str, Any]:
    value = stats_values(stat)
    if by is StatsBy.PROJECT:
        project = split_project(stat.name)
        value["group"], value["project"] = project or ("@unmapped", stat.name)
    elif by is StatsBy.GROUP:
        value["group"] = stat.name
    else:
        value["workdir"] = stat.name
    return value


def stats_total_json(stat: Stat, count: int, by: StatsBy) -> dict[str, Any]:
    value = stats_values(stat)
    key = {
        StatsBy.PROJECT: "projects",
        StatsBy.GROUP: "groups",
        StatsBy.WORKDIR: "workdirs",
    }[by]
    value[key] = count
    return value


def stats_values(stat: Stat) -> dict[str, Any]:
    return {
        "sessions": stat.sessions,
        "usage": usage_json(stat.usage),
        "cost_usd": cost_json(stat.cost),
        "active_seconds": int(stat.active_time.total_seconds()),
        "first_activity": timestamp_json(stat.first_activity),
        "last_activity": timestamp_json(stat.last_activity),
    }


def cost_json(value: float) -> float:
    return math.floor(value * 1_000_000_000_000 + 0.5) / 1_000_000_000_000


def timestamp_json(value: datetime | None) -> str | None:
    if value is None:
        return None
    value = value.astimezone(UTC)
    timespec = "seconds" if value.microsecond == 0 else "microseconds"
    return value.isoformat(timespec=timespec)


def usage_row(
    label: str,
    usage: Usage,
    cost: float,
    active: timedelta | None,
) -> list[str]:
    return [
        label,
        format_tokens(usage.input),
        format_tokens(usage.output),
        format_tokens(usage.total),
        format_cost(cost),
        format_duration(active) if active is not None else "",
    ]


def stats_rows(stats: list[Stat], by: StatsBy) -> list[list[str]]:
    if by is not StatsBy.PROJECT:
        return [stats_row(x, by) for x in stats]

    rows = [stats_row(x, by) for x in stats if split_project(x.name)]
    unmapped = [x for x in stats if split_project(x.name) is None]
    if unmapped:
        total = combine_stats(unmapped)
        total.name = "@unmapped"
        rows.append(stats_row(total, by))
        rows.extend(stats_row(x, by, nested=True) for x in unmapped)
    return rows


def stats_row(stat: Stat, by: StatsBy, nested: bool = False) -> list[str]:
    if by is StatsBy.PROJECT:
        label = stat.name if split_project(stat.name) else workdir_label(stat.name)
    elif by is StatsBy.GROUP:
        label = stat.name
    else:
        label = workdir_label(stat.name)
    if nested:
        label = f"  {label}"
    return [
        label,
        str(stat.sessions),
        format_tokens(stat.usage.total),
        local_date(stat.last_activity) if stat.last_activity else "-",
        format_cost(stat.cost),
        format_duration(stat.active_time),
    ]


def workdir_label(workdir: str) -> str:
    value = os.environ.get("HOME") or os.environ.get("USERPROFILE") or "."
    home = Path(value)
    path = Path(workdir)
    try:
        relative = path.relative_to(home)
    except ValueError:
        return workdir
    return "~" if not relative.parts else f"~/{relative}"


def local_date(value: datetime) -> str:
    return str(value.astimezone().date())


def format_tokens(value: int) -> str:
    if value >= 1_000_000_000:
        return f"{value / 1_000_000_000:.2f}B"
    if value >= 1_000_000:
        return f"{value / 1_000_000:.2f}M"
    if value >= 1_000:
        return f"{value / 1_000:.1f}K"
    return str(value)


def format_cost(value: float) -> str:
    return f"${value:.2f}"


def format_duration(value: timedelta) -> str:
    minutes = (int(value.total_seconds()) + 30) // 60
    return f"{minutes // 60}h {minutes % 60:02}m"
