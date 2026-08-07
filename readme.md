# cctrail

<div align="center">

[<img src="https://badges.ws/pypi/v/cctrail" alt="version" />](https://pypi.org/project/cctrail)
[<img src="https://badges.ws/pypi/python/cctrail" alt="py versions" />](https://pypi.org/project/cctrail)
[<img src="https://badges.ws/pypi/dm/cctrail" alt="downloads" />](https://pypi.org/project/cctrail)
[<img src="https://badges.ws/github/license/vladkens/cctrail" alt="license" />](https://github.com/vladkens/cctrail/blob/main/LICENSE)
[<img src="https://badges.ws/badge/-/buy%20me%20a%20coffee/ff813f?icon=buymeacoffee&label" alt="donate" />](https://buymeacoffee.com/vladkens)

</div>

`cctrail` reads local Claude Code and Codex history and reports token usage, estimated cost, and project statistics. It can also be used as a Python library for working with parsed sessions, messages, tool calls, and agents.

## CLI

Show token usage by day:

```sh
uvx cctrail usage
```

Use named calendar periods and group longer reports by week or month:

```sh
uvx cctrail usage today
uvx cctrail usage last-week
uvx cctrail usage this-month --by week
uvx cctrail usage all --by month
```

Available periods are `today`, `yesterday`, `this-week`, `last-week`, `this-month`, `last-month`, and `all`. Weeks follow the first weekday of the system locale.

Show a per-model breakdown or the detailed reasoning and cache token columns:

```sh
uvx cctrail usage this-month --models
uvx cctrail usage this-month --models --full
```

For an arbitrary date range, use `--since` and `--until`:

```sh
uvx cctrail usage --since 2026-07-01 --until 2026-07-31
```

Show usage grouped by project directory:

```sh
uvx cctrail projects this-month
```

Projects accept the same named periods and arbitrary date ranges as usage reports.

### Configuration

Project aliases can combine a project's main checkout and worktrees into a single project in the report. This is optional: without an alias, `cctrail` reports each path separately.

```toml
[aliases]
cctrail = ["~/Code/vibe/cctrail", "~/.worktrees/cctrail--*"]
```

Alias paths support `~` and glob patterns. `*` matches within one path segment, while `**` can match across directories. For example, `~/.worktrees/cctrail--*` matches sibling worktrees and `~/Code/**/cctrail` matches the project at any depth below `~/Code`.

`cctrail` looks for configuration in this order:

1. The file specified by `CCTRAIL_CONFIG`, if set
2. `./cctrail.toml`
3. `~/.config/cctrail.toml`

Claude data is read from `~/.config/claude` and `~/.claude`, while Codex data is read from `~/.codex`. Set `CLAUDE_CONFIG_DIR` or `CODEX_HOME` to replace these defaults. Both variables accept comma-separated directories:

```sh
CLAUDE_CONFIG_DIR="$HOME/.claude,$HOME/.claude-qh" uvx cctrail usage
CODEX_HOME="$HOME/.codex,$HOME/.codex-work" uvx cctrail usage
```

Usage reports count each provider message once, while parsed sessions retain their original message and sub-agent structure. Costs are estimates based on the pricing catalog embedded in the installed cctrail version.

Active time is estimated by treating every activity event as a five-minute window and merging overlaps, including activity from concurrent sessions.

## Python library

Add `cctrail` to a project:

```sh
uv add cctrail
```

Iterate over parsed sessions:

```python
from cctrail import iter_sessions, tool_call_counts

for session in iter_sessions():
    print(session.provider, session.cwd, session.title, session.usage.total)

    for call in session.iter_tool_calls():
        print(call.name, call.input, call.duration)

    for agent in session.iter_agents():
        print(agent.agent_type, agent.usage.total)

    print(tool_call_counts(session))
```

Provider-specific iterators are available as `iter_claude_sessions()` and `iter_codex_sessions()`. Sub-agent transcripts are attached to the tool call that launched them.

The main data types are `Session`, `Message`, `ToolCall`, and `Usage`. Every model supports `.as_dict()` and `.as_json()`.

```python
from cctrail import Usage, by_project, calculate_cost, get_pricing, iter_sessions, usage_by_period

sessions = list(iter_sessions())
weekly = usage_by_period(sessions, granularity="week")
projects = by_project(sessions)

pricing = get_pricing("claude-sonnet-5")
cost = calculate_cost("claude-sonnet-5", Usage(input=1_000, output=100))
```

Usage is grouped in the system timezone by default. `usage_by_period()` accepts `day`, `week`, and `month` granularities, while `usage_by_day()` remains the daily shortcut. Pass a `datetime.tzinfo` as the `timezone` argument to use another timezone. `UsageSummary` values expose `.cost`, `.active_time`, and per-model `.models`; `ProjectSummary` values expose `.cost` and `.active_time`.

## Development

The project requires Python 3.11 or newer and uses [uv](https://docs.astral.sh/uv/).

```sh
uv sync
make check
make test
```

## License

Distributed under the [MIT License](LICENSE).
