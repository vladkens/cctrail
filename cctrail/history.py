from collections.abc import Iterable, Iterator
from pathlib import Path

from .claude import iter_sessions_with_pool as iter_claude_sessions_with_pool
from .codex import iter_sessions_with_pool as iter_codex_sessions_with_pool
from .models import Session
from .utils import WorkerPool


def iter_sessions(
    root: str | Path | None = None,
    *,
    claude_roots: Iterable[str | Path] | None = None,
    codex_roots: Iterable[str | Path] | None = None,
) -> Iterator[Session]:
    """Stream Claude Code and Codex sessions.

    Without a root, provider directories come from `CLAUDE_CONFIG_DIR` and
    `CODEX_HOME`, falling back to their standard locations. When a root is
    provided, both parsers inspect that tree and ignore files that belong to
    the other provider.
    """
    if root is not None and (claude_roots is not None or codex_roots is not None):
        raise ValueError("root cannot be combined with provider roots")

    with WorkerPool() as pool:
        if root is not None:
            yield from iter_claude_sessions_with_pool(pool, root=root)
            yield from iter_codex_sessions_with_pool(pool, root=root)
            return

        yield from iter_claude_sessions_with_pool(pool, roots=claude_roots)
        yield from iter_codex_sessions_with_pool(pool, roots=codex_roots)
