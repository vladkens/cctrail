import json
import os
import signal
from collections.abc import Callable, Collection, Iterable, Iterator
from datetime import datetime
from multiprocessing import cpu_count, get_context
from pathlib import Path
from typing import Any, TypeVar

import orjson

T = TypeVar("T")
R = TypeVar("R")
PARALLEL_MIN_ITEMS = 32


class WorkerConfigError(ValueError):
    pass


def ignore_sigint() -> None:
    signal.signal(signal.SIGINT, signal.SIG_IGN)


def data_paths(variable: str, defaults: Iterable[Path]) -> list[Path]:
    value = os.environ.get(variable)
    if value is None:
        paths = list(defaults)
    else:
        paths = [Path(item.strip()).expanduser() for item in value.split(",") if item.strip()]
    return list(dict.fromkeys(paths))


def history_roots(
    root: str | Path | None,
    roots: Iterable[str | Path] | None,
    defaults: Iterable[Path],
) -> list[Path]:
    if root is not None and roots is not None:
        raise ValueError("root and roots cannot be combined")
    if roots is not None:
        return [Path(item).expanduser() for item in roots]
    if root is not None:
        return [Path(root).expanduser()]
    return list(defaults)


class WorkerPool:
    def __init__(self) -> None:
        configured = os.environ.get("CCTRAIL_WORKERS", "").strip()
        self._explicit = bool(configured)
        workers = cpu_count()
        if configured:
            try:
                workers = int(configured)
            except ValueError:
                raise WorkerConfigError("CCTRAIL_WORKERS must be a positive integer") from None
            if workers < 1:
                raise WorkerConfigError("CCTRAIL_WORKERS must be a positive integer")

        self._workers = min(workers, cpu_count())
        self._pool = None

    def __enter__(self) -> "WorkerPool":
        return self

    def __exit__(self, exc_type, _exc, _traceback) -> None:
        if self._pool is None:
            return
        if exc_type is None:
            self._pool.close()
        else:
            self._pool.terminate()
        self._pool.join()

    def map(
        self,
        func: Callable[[T], R],
        iterable: Iterable[T],
        *,
        chunksize: int = 1,
    ) -> list[R]:
        items = list(iterable)
        if not items:
            return []

        if self._workers == 1 or (not self._explicit and len(items) < PARALLEL_MIN_ITEMS):
            return list(map(func, items))

        if self._pool is None:
            self._pool = get_context("spawn").Pool(
                self._workers,
                initializer=ignore_sigint,
            )
        return list(self._pool.imap(func, items, chunksize))


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
