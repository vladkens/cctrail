import os
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import orjson

from ..model import Provider, Session


@dataclass
class AgentCall:
    tool_id: str
    agent_id: str | None = None
    resolved: bool = False


def parse_history(provider: Provider | None = None) -> list[Session]:
    from . import claude, codex

    sessions = []
    if provider is not Provider.CODEX:
        sessions.extend(claude.parse(history_root("CLAUDE_CONFIG_DIR", ".claude")))
    if provider is not Provider.CLAUDE:
        sessions.extend(codex.parse(history_root("CODEX_HOME", ".codex")))
    return sessions


def history_root(variable: str, fallback: str) -> Path:
    value = os.environ.get(variable)
    return Path(value) if value is not None else home_dir() / fallback


def home_dir() -> Path:
    value = os.environ.get("HOME") or os.environ.get("USERPROFILE")
    return Path(value) if value else Path(".")


def jsonl_files(roots: list[Path]) -> list[Path]:
    return sorted(
        path for root in roots if root.is_dir() for path in root.rglob("*.jsonl") if path.is_file()
    )


def jsonl(path: Path) -> Iterator[dict[str, Any]]:
    try:
        with path.open("rb") as file:
            for line in file:
                try:
                    value = orjson.loads(line)
                except orjson.JSONDecodeError:
                    continue
                if isinstance(value, dict):
                    yield value
    except OSError:
        return


def inherit_roots(sessions: list[Session], parents: dict[int, int]) -> None:
    for child in range(len(sessions)):
        root = child
        for _ in sessions:
            if root not in parents:
                break
            root = parents[root]
        sessions[child].root_id = sessions[root].id
        sessions[child].workdir = sessions[root].workdir
