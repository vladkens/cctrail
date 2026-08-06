from collections.abc import Iterator
from pathlib import Path

from .claude import iter_sessions as iter_claude_sessions
from .codex import iter_sessions as iter_codex_sessions
from .models import Session


def iter_sessions(root: str | Path | None = None) -> Iterator[Session]:
    """Stream Claude Code and Codex sessions.

    Without a root, provider directories come from `CLAUDE_CONFIG_DIR` and
    `CODEX_HOME`, falling back to their standard locations. When a root is
    provided, both parsers inspect that tree and ignore files that belong to
    the other provider.
    """
    if root is None:
        yield from iter_claude_sessions()
        yield from iter_codex_sessions()
        return

    yield from iter_claude_sessions(root)
    yield from iter_codex_sessions(root)
