# cctrail

<div align="center">

[<img src="https://badges.ws/pypi/v/cctrail" alt="version" />](https://pypi.org/project/cctrail)
[<img src="https://badges.ws/pypi/python/cctrail" alt="py versions" />](https://pypi.org/project/cctrail)
[<img src="https://badges.ws/pypi/dm/cctrail" alt="downloads" />](https://pypi.org/project/cctrail)
[<img src="https://badges.ws/github/license/vladkens/cctrail" alt="license" />](https://github.com/vladkens/cctrail/blob/main/LICENSE)
[<img src="https://badges.ws/badge/-/buy%20me%20a%20coffee/ff813f?icon=buymeacoffee&label" alt="donate" />](https://buymeacoffee.com/vladkens)

</div>

`cctrail` reads local Claude Code and Codex history and reports token usage, estimated cost, and project statistics. It can also be used as a Python library for working with parsed sessions, messages, tool calls, and agents.

```console
$ uvx cctrail groups today
Group     Projects Sessions Tokens Last active   Cost   Time
exp              2        2 32.36M 2026-08-09  $24.00 1h 15m
wrk              1        1 24.93M 2026-08-09  $19.45 0h 49m
ungrouped        2        4 20.56M 2026-08-09  $17.31 1h 26m
oss              1        1 10.35M 2026-08-09   $7.90 0h 31m
comms            1        1  71.1K 2026-08-09   $0.10 0h 06m
total            7        9 88.27M 2026-08-09  $68.74 4h 08m
```

```console
$ uvx cctrail projects today
Project             Sessions Total tokens Last active   Cost   Time
~/Code/vibe/cctrail        1       32.04M 2026-08-09  $23.43 1h 10m
atlas                      1       24.93M 2026-08-09  $19.45 0h 49m
starship                   2       17.87M 2026-08-09  $14.37 1h 02m
~/Code/pub/macmon          1       10.35M 2026-08-09   $7.90 0h 31m
~/Code/sideproj            2        2.68M 2026-08-09   $2.93 0h 24m
~/Code/labs                1       322.1K 2026-08-09   $0.57 0h 05m
~/Code/client-app          1        71.1K 2026-08-09   $0.10 0h 06m
total                      9       88.27M 2026-08-09  $68.74 4h 08m
```

```console
$ uvx cctrail usage this-month
Date        Input Output   Total     Cost    Time
2026-08-01  5.75M  1.30M 281.22M  $174.00 12h 45m
2026-08-02  6.93M 780.8K 204.76M  $276.40 10h 24m
2026-08-03  4.88M 338.4K 138.49M  $193.10  6h 02m
2026-08-04  4.55M 693.6K 244.77M   $92.25  6h 39m
2026-08-05  4.43M 444.6K 166.19M  $102.47  6h 06m
2026-08-06  4.52M 226.4K  93.27M   $83.17  6h 22m
2026-08-07  6.15M 631.3K 232.52M  $153.22  9h 14m
2026-08-08  24.5K   4.0K  123.3K    $0.31  0h 07m
2026-08-09  3.06M 196.9K  92.68M   $72.05  4h 14m
total      40.30M  4.61M   1.45B $1146.97 61h 56m
```

## Why cctrail?

Before coding agents, I tracked time with a VS Code extension. That worked while most of the job happened inside the editor. In the new agentic coding era, much of my work happens in terminal sessions instead: I start an agent, review its results, send another prompt, and move between several projects. The editor no longer sees enough of that activity to answer a simple question: where did my time go?

I tried several ways to recover that information, and local agent history turned out to be the most useful source. `cctrail` turns it into reports that help explore how agent-assisted work is distributed over time, projects, and configurable groups. Its purpose is broader than any single report: to make the work recorded by coding agents visible and understandable from different perspectives.

[ccusage](https://github.com/ccusage/ccusage) is an excellent alternative for ready-made token and cost reports across many coding agents. `cctrail` grew from a different question: not only how many tokens were used, but what I worked on and how my time was divided.

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
Paths inside your home directory are shortened to `~/…`; pass `--full` to show absolute paths.

Show usage aggregated across configured project groups:

```sh
uvx cctrail groups this-month
uvx cctrail groups --provider codex
uvx cctrail groups --json
```

Groups accept the same periods, date ranges, and provider filter as project reports. Projects that do not match a configured group are included under `ungrouped`.

### Configuration

Project aliases can combine a project's main checkout and worktrees into a single project in the report. This is optional: without an alias, `cctrail` reports each path separately.

```toml
[aliases]
cctrail = ["~/Code/vibe/cctrail", "~/.worktrees/cctrail--*"]

[groups]
personal = ["cctrail", "~/Code/dotfiles"]
opensource = ["~/Code/pub/**"]
```

Alias paths support `~` and glob patterns. `*` matches within one path segment, while `**` can match across directories. A trailing `/**` includes both the named directory and everything below it. For example, `~/.worktrees/cctrail--*` matches sibling worktrees and `~/Code/**/cctrail/**` matches the project at any depth below `~/Code` together with all of its checkouts.

Groups are resolved after aliases, so a group can contain an alias such as `cctrail`, an exact project path, or a glob pattern. A project can match only one group.

Use `--config` to select a file for one command, or `--no-config` to ignore every TOML configuration:

```sh
uvx cctrail --config ~/configs/work.toml projects
uvx cctrail --no-config projects
```

Configuration is selected in this order:

1. `--no-config` disables configuration
2. `--config PATH` selects a specific file
3. `./cctrail.toml`
4. `~/.config/cctrail.toml`

An explicitly selected file must exist. The automatic locations are optional.

You can also configure every Claude Code and Codex history directory instead of setting `CLAUDE_CONFIG_DIR` or `CODEX_HOME` for each command:

```toml
[sources]
claude = ["~/.claude", "~/.claude-work"]
codex = ["~/.codex", "~/.codex-work"]
```

When a provider is present in `[sources]`, its list replaces the default directories. Environment variables still take precedence when set.

Claude data is read from `~/.config/claude` and `~/.claude`, while Codex data is read from `~/.codex`. Set `CLAUDE_CONFIG_DIR` or `CODEX_HOME` to replace these defaults. Both variables accept comma-separated directories:

```sh
CLAUDE_CONFIG_DIR="$HOME/.claude,$HOME/.claude-qh" uvx cctrail usage
CODEX_HOME="$HOME/.codex,$HOME/.codex-work" uvx cctrail usage
```

Claude Code deletes session files older than 30 days at startup by default. Because `cctrail` reads those local transcripts, deleted sessions also disappear from its reports. To preserve a long-term history, increase [`cleanupPeriodDays`](https://code.claude.com/docs/en/settings) in `~/.claude/settings.json` before the default cleanup removes it:

```json
{
  "cleanupPeriodDays": 9999
}
```

Usage reports count each provider message once, while parsed sessions retain their original message and sub-agent structure. Costs are estimates based on the pricing catalog embedded in the installed cctrail version.

Active time is estimated by treating every activity event as a five-minute window. Sessions without token usage in the selected period are ignored. Overlapping sessions within a project are merged; when several projects are active at once, that time is divided evenly between them. This keeps the project and group totals equal to the overall active time.

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
