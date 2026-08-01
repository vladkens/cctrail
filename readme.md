# cctrail

Streaming parser for coding agent chat history (Claude Code today, Codex later) into
structured, typed sessions — for building your own usage reports, dashboards or analysis
on top.

Give it a folder, get back `Session` objects one at a time: tokens (input/output,
cache read/write), tool calls (name, input, output, timing), and the full sub-agent
tree (`Agent`/`Task` tool calls resolved into nested sessions with their own usage and
duration), without loading the whole history into memory at once.

## Usage

```python
from cctrail import iter_claude_sessions

for session in iter_claude_sessions():  # defaults to ~/.claude/projects
    print(session.cwd, session.title, session.usage.total)

    for call, count in session.tool_call_counts.items():
        print(f"  {call}: {count}")

    for agent in session.walk():  # session itself, then every nested agent run
        print(f"  agent {agent.agent_type}: {agent.usage.total} tokens, {agent.duration}")
```

Point it at any directory tree instead of the default:

```python
for session in iter_claude_sessions("/path/to/backup/.claude/projects"):
    ...
```

## Model

- `Usage` — `input` / `cache_write` / `cache_read` / `output` token counts, plus `.total`.
- `ToolCall` — `name`, `input`, `output_text`, `is_error`, `started_at`/`ended_at`/`duration`,
  and `raw_result` (Claude Code's raw `toolUseResult`, e.g. `toolStats` for `Agent` calls).
- `Message` — one user or assistant turn: `text`, `usage`, `tool_calls`, and for user turns
  the harness-reported `turn_duration` (how long the agent took to answer).
- `Session` — a conversation: `messages`, `usage`, `tool_calls`, `tool_call_counts`,
  `duration`, and `agents` (sub-agent runs launched from this session, each a `Session`
  of `kind="agent"`). `.walk()` flattens the whole tree.

`cctrail.metrics.active_time(timestamps)` turns a list of event timestamps into a
merged "actually active" duration (bursts of activity within a 5-minute window count
once), useful for "how much time went into this project" style rollups across sessions.

## Status

Claude Code only for now. Codex support is planned as a second `cctrail.codex` module
behind the same `Session`/`Message`/`ToolCall` model.
