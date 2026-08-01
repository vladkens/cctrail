import dataclasses
import json
from datetime import date, datetime, timedelta
from inspect import getmembers
from typing import Any


def as_dict(obj: Any) -> Any:
    """Recursively convert a dataclass (fields *and* `@property` values) into plain dicts/lists.

    Unlike `dataclasses.asdict`, this also walks computed properties (e.g.
    `Session.usage`, `Session.tool_call_counts`), so derived metrics survive
    the round trip to JSON, not just the raw parsed fields.
    """
    if dataclasses.is_dataclass(obj) and not isinstance(obj, type):
        result = {f.name: as_dict(getattr(obj, f.name)) for f in dataclasses.fields(obj)}
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
    return json.dumps(as_dict(obj), **kwargs)
