import json
import os
import signal
from collections.abc import Callable, Collection, Iterable, Iterator
from datetime import datetime
from multiprocessing import cpu_count, get_all_start_methods, get_context
from pathlib import Path
from typing import Any, TypeVar

import orjson

T = TypeVar("T")
R = TypeVar("R")


def ignore_sigint() -> None:
    signal.signal(signal.SIGINT, signal.SIG_IGN)


def data_paths(variable: str, defaults: Iterable[Path]) -> list[Path]:
    value = os.environ.get(variable)
    if value is None:
        paths = list(defaults)
    else:
        paths = [Path(item.strip()).expanduser() for item in value.split(",") if item.strip()]
    return list(dict.fromkeys(paths))


def imap(
    func: Callable[[T], R],
    iterable: Iterable[T],
    *,
    workers: int | None = None,
    chunksize: int = 1,
) -> Iterator[R]:
    items = list(iterable)
    if not items:
        return

    workers = min(workers or cpu_count(), len(items))
    if workers == 1 or "fork" not in get_all_start_methods():
        yield from map(func, items)
        return

    with get_context("fork").Pool(workers, initializer=ignore_sigint) as pool:
        yield from pool.imap(func, items, chunksize)


def json_load(value: str | bytes, default: Any = None) -> Any:
    try:
        return orjson.loads(value)
    except orjson.JSONDecodeError:
        return default


def json_dump(value: Any, **kwargs: Any) -> str:
    if kwargs:
        return json.dumps(value, **kwargs)

    return orjson.dumps(value).decode()


def json_lines(path: Path) -> Iterator[dict[str, Any]]:
    try:
        with path.open("rb") as fh:
            for line in fh:
                value = json_load(line)
                if isinstance(value, dict):
                    yield value
    except OSError:
        return


def parse_timestamp(value: Any) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None

    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def string_or_none(value: Any) -> str | None:
    return value if isinstance(value, str) and value else None


def int_or_zero(value: Any) -> int:
    if isinstance(value, bool):
        return 0

    if isinstance(value, int):
        return max(0, value)

    if isinstance(value, float) and value.is_integer():
        return max(0, int(value))

    return 0


def extract_text(content: Any, block_types: Collection[str]) -> str:
    if isinstance(content, str):
        return content

    if not isinstance(content, list):
        return ""

    return "\n".join(
        block["text"]
        for block in content
        if isinstance(block, dict)
        and block.get("type") in block_types
        and isinstance(block.get("text"), str)
    )
