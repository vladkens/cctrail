# cctrail

<div align="center">

[<img src="https://badges.ws/crates/v/cctrail" alt="version" />](https://crates.io/crates/cctrail)
[<img src="https://badges.ws/github/license/vladkens/cctrail" alt="license" />](https://github.com/vladkens/cctrail/blob/main/LICENSE)
[<img src="https://badges.ws/badge/-/buy%20me%20a%20coffee/ff813f?icon=buymeacoffee&label" alt="donate" />](https://buymeacoffee.com/vladkens)

</div>

`cctrail` reads local Claude Code and Codex history and shows, per project, the active time,
tokens and estimated cost.

## Install

```sh
cargo install cctrail
```

Or take a prebuilt binary for macOS, Linux or Windows from the
[releases](https://github.com/vladkens/cctrail/releases).

## Usage

```sh
cctrail                    # statistics per project
cctrail this-month         # for a period
cctrail --by group         # per group, or --by dir per working directory
cctrail here               # the current directory's project: today, this week, this month…
cctrail days               # tokens, cost and time per day; also weeks, months
cctrail weeks --models     # broken down by model
```

Periods are `today`, `yesterday`, `this-week`, `last-week`, `this-month`, `last-month` and `all`
(the default); weeks start on Monday. `--since` and `--until` take inclusive `YYYY-MM-DD` dates
instead. `--provider claude|codex` reads one harness only, and `--json` prints machine-readable
output with full paths and token counts.

### Projects

Projects are configured in `~/.config/cctrail/config.toml`. `XDG_CONFIG_HOME` changes the config
root and `CCTRAIL_CONFIG` overrides the complete path.

```toml
[projects]
"env/dotfiles" = [
  "~/Code/dotfiles",
  "~/Code/scripts",
]
"oss/cctrail" = ["~/Code/cctrail"]
```

Each key has the `<group>/<project>` form. A path includes all directories below it; when paths
overlap, the longest match wins. Directories outside any project are summed up as `@unmapped`;
`cctrail --by dir` lists them.

## How it counts

Claude history is read from `~/.claude/projects` and Codex history from `~/.codex/sessions` and
`~/.codex/archived_sessions`; `CLAUDE_CONFIG_DIR` and `CODEX_HOME` override those roots. Claude Code
deletes sessions older than 30 days at startup by default: raise
[`cleanupPeriodDays`](https://code.claude.com/docs/en/claude-directory#cleaned-up-automatically) to
keep a long history.

Active time treats every user or assistant event as a five-minute window. Overlapping windows
merge, and time spent in several projects at once is divided between them.

Costs are estimates from the [models.dev](https://models.dev) prices bundled with the release.
`cctrail update-pricing` downloads the latest ones to `~/.cache/cctrail/pricing.json`, which then
take precedence. Tokens on models without a price are listed below the report.

## License

Distributed under the [MIT License](LICENSE).
