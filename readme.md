# cctrail

`cctrail` is a Python library and a small CLI for reading local Claude Code and Codex history and turning it into structured sessions, messages, tool calls, token usage, and project statistics.

## Python API

```python
from cctrail import iter_sessions

for session in iter_sessions():
    print(session.provider, session.cwd, session.title, session.usage.total)

    for call in session.tool_calls:
        print(call.name, call.input, call.duration)

    for agent in session.agents:
        print(agent.agent_type, agent.usage.total)
```

By default, sessions are read from `~/.claude/projects` and `~/.codex`. A specific history directory can be passed explicitly:

```python
sessions = iter_sessions("/path/to/history")
```

Provider-specific iterators are also available as `iter_claude_sessions()` and `iter_codex_sessions()`. Sub-agent transcripts are resolved automatically and attached to the tool call that launched them.

The main data types are `Session`, `Message`, `ToolCall`, and `Usage`. Parsed objects can be converted to dictionaries or JSON with `as_dict()` and `as_json()`.

```python
from cctrail import as_json, iter_sessions

session = next(iter_sessions())
print(as_json(session, indent=2))
```

## CLI

Show token usage by day:

```sh
uv run cctrail usage
uv run cctrail usage --since 2026-07-01 --until 2026-07-31
```

Show usage grouped by project directory:

```sh
uv run cctrail projects
```

Both commands combine Claude Code and Codex history by default and accept `--root` for another history directory.

Usage reports count each provider message once, while parsed sessions retain their original message and sub-agent structure.

## Development

The project requires Python 3.11 or newer and uses [uv](https://docs.astral.sh/uv/).

```sh
uv sync
make check
make test
```

## License

Distributed under the [MIT License](LICENSE).
