<div align="center">

# cctrail

**Where your AI coding time and tokens go, per project**

[<img src="https://badges.ws/crates/v/cctrail" alt="version" />](https://crates.io/crates/cctrail)
[<img src="https://badges.ws/github/license/vladkens/cctrail" alt="license" />](https://github.com/vladkens/cctrail/blob/main/LICENSE)

</div>

`cctrail` reads the local history of Claude Code and Codex and shows, for each project, how much
time you spent in it, how many tokens it used and what that would cost at API prices. It needs no
setup: repositories are found from the sessions themselves, with agent worktrees and clones counted
as one project.

## Install

```sh
cargo install cctrail
```

Or take a prebuilt binary for macOS, Linux or Windows from the
[releases](https://github.com/vladkens/cctrail/releases).

## Usage

```sh
cctrail                  # on a terminal: the interactive view (projects, models, timeline)
cctrail all              # time, tokens and cost per project as text, all time
cctrail this-month       # for a period: today, yesterday, this-week, last-week, this-month, last-month
cctrail --all            # every row; by default rows past 95% of the time are summed up as `other`
cctrail --by group       # per group from the config, or --by dir per working directory
cctrail here             # the current directory's project: today, this week, this month, all time
cctrail days             # per day; also weeks, months, and --models for a breakdown by model
```

`--since` and `--until` take inclusive `YYYY-MM-DD` dates instead of a period, `--provider
claude|codex` reads one harness only, and `--json` prints everything with full paths and exact
counts. Weeks start on Monday. `cctrail --help` lists it all.

`cctrail` without arguments on a terminal opens the interactive view: projects, models and a
timeline (a chart of the days stacked by model over their list) for today, this week, this month,
the last 30 days or all time; `t` and `T` switch the period. `→` or `enter` opens a row (a
project's models, a model's speeds and efforts, a day's projects) and `←`, `esc` or `q` goes back,
`s` sorts by the next column and `S` reverses (on the timeline `s` switches the bars between time,
tokens and cost), `p` switches the provider, `/` filters, `?` lists the keys. The mouse works too:
a click selects a row or a bar and a second one opens it, a column header sorts, a click on a
view, period or provider switches it, the wheel scrolls. Any argument, `--json` or a pipe prints
the text report instead.

## Projects

Every repository is a row, named by its directory. Agent worktrees count as their repository
(`<repo>/.claude/worktrees/<name>`, any `<repo>/.<tool>/worktrees/<name>`,
`<repo>/.worktrees/<name>` and `<repo>-worktrees/<name>`), and clones of one repository at other
paths (directories with the same name) count as the one used last. Temp directories, directories
that hold other projects (`~/Code`) and clones Codex saw with different git remotes stay apart.

A config is optional: it names projects, merges several repositories into one and puts projects in
groups for `--by group`. It lives in `~/.config/cctrail/config.toml` (`XDG_CONFIG_HOME` moves the
root, `CCTRAIL_CONFIG` sets the full path):

```toml
[projects]
"env/dotfiles" = ["~/Code/dotfiles", "~/Code/scripts"]
"oss/cctrail" = ["~/Code/cctrail"]
```

Each key is `<group>/<project>`. A path includes everything below it, and the longest match wins.
Repositories outside the config keep their own rows and form the `@unmapped` group.

## How it counts

- **History:** Claude Code from `~/.claude/projects`, Codex from `~/.codex/sessions` and
  `~/.codex/archived_sessions`, plus the same in every profile beside them (`~/.claude-work`,
  `~/.codex-personal`). `CLAUDE_CONFIG_DIR` and `CODEX_HOME` replace all of them with one root.
  Claude Code deletes sessions older than 30 days by default: raise
  [`cleanupPeriodDays`](https://code.claude.com/docs/en/claude-directory#cleaned-up-automatically)
  to keep a long history.
- **Time:** every message opens a five-minute window; overlapping windows merge, and time spent in
  several projects at once is split between them. Tables show `8h 14m`, or `45d 8h 14m` from a day
  on.
- **Cost:** an estimate at [models.dev](https://models.dev) prices, each request at the price of
  its day, so a later price cut doesn't reprice old usage. Prices come from the models.dev git
  history and are refreshed in the background once a day (`cctrail update-pricing` does it now,
  `CCTRAIL_NO_UPDATE=1` turns it off). `codex-auto-review`, which has no price, counts as
  `gpt-6-luna`; tokens on other unpriced models are listed below the report.

## License

Distributed under the [MIT License](LICENSE).
