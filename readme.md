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
cctrail                    # statistics per project and unconfigured repository
cctrail this-month         # for a period
cctrail --by group         # per group, or --by dir per working directory
cctrail --all              # every row, nothing summed up as other
cctrail here               # the current directory's project: today, this week, this month…
cctrail days               # tokens, cost and time per day; also weeks, months
cctrail weeks --models     # broken down by model
```

Periods are `today`, `yesterday`, `this-week`, `last-week`, `this-month`, `last-month` and `all`
(the default); weeks start on Monday. `--since` and `--until` take inclusive `YYYY-MM-DD` dates
instead. `--provider claude|codex` reads one harness only, and `--json` prints machine-readable
output with full paths and token counts.

### Projects

Without a config, `cctrail` lists every repository by its directory, dimmed, until the rows cover
95% of the time; the rest is summed up as `other` (`--all` lists them all; `--by dir` folds the
same way). A config is optional: it names repositories, merges several into one project and puts
projects in groups for `--by group`. It
lives in `~/.config/cctrail/config.toml`; `XDG_CONFIG_HOME` changes the config root and
`CCTRAIL_CONFIG` overrides the complete path.

```toml
[projects]
"env/dotfiles" = [
  "~/Code/dotfiles",
  "~/Code/scripts",
]
"oss/cctrail" = ["~/Code/cctrail"]
```

Each key has the `<group>/<project>` form. A path includes all directories below it; when paths
overlap, the longest match wins. Directories outside any project keep their own rows, and count as
the `@unmapped` group in `--by group` and in JSON.

Agent worktrees count as their repository, recognized by their path:
`<repo>/.claude/worktrees/<name>` (any `<repo>/.<tool>/worktrees/<name>`),
`<repo>/.worktrees/<name>` and `<repo>-worktrees/<name>`. A path configured for a single worktree
still maps it on its own. Clones of a repository at other paths (directories of the same name)
count as the one used last, except temp directories, directories holding other projects
(`~/Code`) and clones Codex saw with different git remotes. `--json --by dir` lists the folded
paths of each directory.

## How it counts

Claude history is read from `~/.claude/projects` and Codex history from `~/.codex/sessions` and
`~/.codex/archived_sessions`, and the same from every profile beside them (`~/.claude-work`,
`~/.codex-personal`); `CLAUDE_CONFIG_DIR` and `CODEX_HOME` replace all of those with one root.
Claude Code deletes sessions older than 30 days at startup by default: raise
[`cleanupPeriodDays`](https://code.claude.com/docs/en/claude-directory#cleaned-up-automatically) to
keep a long history.

Active time treats every user or assistant event as a five-minute window. Overlapping windows
merge, and time spent in several projects at once is divided between them. Tables show it as
`8h 14m`, and from a day on as `45d 8h 14m` (days of 24 hours); JSON gives seconds.

Costs are estimates from [models.dev](https://models.dev) prices, each request at the price of its
day: a later price cut doesn't reprice old usage, and a model models.dev drops keeps its last
price. The dated prices come from the models.dev git history, so a change counts from the day
models.dev recorded it, usually a few days after the provider's announcement.
`codex-auto-review`, Codex's approval reviewer, has no price of its own and counts as `gpt-6-luna`.

The prices bundled with the release are refreshed in the background once a day into
`~/.cache/cctrail/pricing.tsv`, while the history is read; `cctrail update-pricing` does it now,
and `CCTRAIL_NO_UPDATE=1` turns the daily check off. Tokens on models without a price are listed
below the report.

## License

Distributed under the [MIT License](LICENSE).
