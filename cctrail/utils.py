import dataclasses
import json
from collections.abc import Callable, Collection, Iterable, Iterator
from datetime import date, datetime, timedelta
from inspect import getmembers
from multiprocessing import cpu_count, get_all_start_methods, get_context
from pathlib import Path
from typing import Any, TypeVar

import orjson

T = TypeVar("T")
R = TypeVar("R")


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

    with get_context("fork").Pool(workers) as pool:
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


def as_dict(obj: Any) -> Any:
    """Recursively convert dataclasses and their properties into plain values."""
    if dataclasses.is_dataclass(obj) and not isinstance(obj, type):
        result = {
            field.name: as_dict(getattr(obj, field.name)) for field in dataclasses.fields(obj)
        }
        for name, _ in getmembers(type(obj), lambda member: isinstance(member, property)):
            if not name.startswith("_"):
                result[name] = as_dict(getattr(obj, name))
        return result

    if isinstance(obj, dict):
        return {scalar(key): as_dict(value) for key, value in obj.items()}

    if isinstance(obj, (list, tuple)):
        return [as_dict(item) for item in obj]

    return scalar(obj)


def scalar(value: Any) -> Any:
    if isinstance(value, timedelta):
        return value.total_seconds()

    if isinstance(value, datetime):
        return value.isoformat()

    if isinstance(value, date):
        return value.isoformat()

    return value


def as_json(obj: Any, **kwargs: Any) -> str:
    return json_dump(as_dict(obj), **kwargs)
