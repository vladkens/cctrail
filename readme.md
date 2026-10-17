<div align="center">

# cctrail

**Where your AI coding time and tokens go, per project**

[<img src="https://badges.ws/crates/v/cctrail" alt="version" />](https://crates.io/crates/cctrail)
[<img src="https://badges.ws/github/license/vladkens/cctrail" alt="license" />](https://github.com/vladkens/cctrail/blob/main/LICENSE)
[<img src="https://badges.ws/badge/follow-@uid127-black" alt="follow @uid127" />](https://x.com/uid127)

</div>

`cctrail` reads the local history of Claude Code, Codex, [pi](https://pi.dev) and
[fx](https://fx.sh) and shows, for each project, how much time you spent in it, how many tokens it
used and what that would cost at API prices. It needs no setup: repositories are found from the
sessions themselves, with agent worktrees and clones counted as one project.

```text
╭─ Projects 1  Models 2  Timeline 3 ────────────────────────────────── cctrail v0.3.0 ─╮
│ Today  Week  Month  30d  All                                      claude+codex+pi+fx │
╰──────────────────────────────────────────────────────────────────────────────────────╯
╭─ projects 5 ─ / filter ─ sort ────────────────────────────────────────────── → open ─╮
│   Project                        Time ↓    Tokens         Cost    Sessions    Share  │
│ ▶ work/web-app                2d 7h 22m    100.4M       $86.85          42      42%  │
│   work/api                   1d 11h 02m     58.7M       $49.32          21      27%  │
│   oss/cli-tool                  23h 14m     38.4M       $33.89          21      18%  │
│   oss/blog                      11h 10m     17.2M       $14.69          20       8%  │
│   /home/me/Code/dotfiles         6h 08m      8.4M        $7.15          21       5%  │
│   total                      5d 10h 55m    223.1M      $191.90         125     100%  │
╰──────────────────────────────────────────────── ? help | quit | t period | provider ─╯
```

## 🌟 Features

- ⏱️ Active time per project, not session length: idle gaps don't count, and time spent in
  several projects at once is split between them
- 💰 Tokens and their cost at [models.dev](https://models.dev) API prices, each request at the
  price of its day
- 🌳 Agent worktrees and clones of a repository fold into one project, with no config
- 🗂️ An optional config to name projects, merge repositories and put them in groups
- 🧩 Claude Code, Codex, pi and fx, including every Claude Code and Codex profile
  (`~/.claude-work`)
- 🖥️ An interactive view of projects, models and a timeline, with drill-downs, keys and mouse
- 📊 Text reports by period, day, week or month, per project, group, directory or model, and JSON
- ⚡ Fast: about 4.5 s for ~11 GB of history on an M-series Mac (2026-10-14)
- 🔒 Local: reads the history only, and goes online only for prices
- 📦 One binary for macOS, Linux and Windows

## 🤖 How it's made

cctrail is written by AI agents (Claude Code, Codex) following [agents.md](agents.md); I choose
what to build and review every pull request. The parsers are tested on small synthetic histories
([fixtures](tests/fixtures/)), every change is run against the previous build on my full local
history so the tokens, sessions and time don't drift, and every speed change is measured and
logged in [docs/perf.md](docs/perf.md). This isn't a weekend prompt: I've written software by hand
for years, such as [macmon](https://github.com/vladkens/macmon) (Rust, Apple Silicon system
monitor), [twscrape](https://github.com/vladkens/twscrape) (Python, X/Twitter scraping) and
[ecloop](https://github.com/vladkens/ecloop) (C, low-level elliptic-curve math), and agents are how
I build now.

## 📥 Installation

Download the archive for macOS (Apple Silicon), Linux (x86_64 or arm64, glibc 2.28 or newer) or
Windows (x86_64) from the [latest release](https://github.com/vladkens/cctrail/releases/latest)
and put `cctrail` (`cctrail.exe`) into a folder on your `PATH`, such as `~/.local/bin`. Or build
it with Rust:

```sh
cargo install cctrail
```

Every archive is attested: `gh attestation verify FILE -R vladkens/cctrail` checks that this
repository's CI built it.

### Newest version

This readme follows main, which may be ahead of the latest release. Binaries of main are in the
[dev release](https://github.com/vladkens/cctrail/releases/tag/dev), or build it with Rust:

```sh
cargo install --git https://github.com/vladkens/cctrail
```

## 🚀 Usage

```sh
cctrail                      # on a terminal: the interactive view (projects, models, timeline)
cctrail all                  # time, tokens and cost per project as text, all time
cctrail this-month           # for a period: today, yesterday, this-week, last-week, this-month, last-month
cctrail --since 2026-03-01   # from a date; --until sets the last day, both inclusive YYYY-MM-DD
cctrail --all                # every row; by default rows past 95% of the time are summed up as `other`
cctrail --by group           # per group from the config, or --by dir per working directory
cctrail here                 # the current directory's project: today, this week, this month, all time
cctrail days                 # per day; also weeks, months, and --models for a breakdown by model
cctrail --provider codex     # one harness only: claude, codex, pi or fx
cctrail --json               # everything with full paths and exact counts
cctrail update-pricing       # download the latest prices now
```

Weeks start on Monday. `cctrail --help` lists it all.

`cctrail` without arguments on a terminal opens the interactive view: projects, models and a
timeline (a chart of the days stacked by model over their list) for today, this week, this month,
the last 30 days or all time; `1`, `2` and `3` switch the view, `t` and `T` the period. `→` or
`enter` opens a row (a project's models, a model's speeds and efforts, a day's projects) and `←`,
`esc` or `q` goes back, `s` sorts by the next column and `S` reverses (on the timeline `s` switches
the bars between time, tokens and cost), `p` switches the provider, `/` filters, `?` lists the
keys. The mouse works too: a click selects a row or a bar and a second one opens it, a column
header sorts, a click on a view, period or provider switches it, the wheel scrolls. Any argument,
`--json` or a pipe prints the text report instead.

<a name="projects"></a>

## 📁 Projects

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
"work/web-app" = ["~/Code/web-app", "~/Code/web-app-docs"]
"oss/cli-tool" = ["~/Code/cli-tool"]
```

Each key is `<group>/<project>`. A path includes everything below it, and the longest match wins.
Repositories outside the config keep their own rows and form the `@unmapped` group.

## 🧮 How it counts

- **History:** Claude Code from `~/.claude/projects`, Codex from `~/.codex/sessions` and
  `~/.codex/archived_sessions`, plus the same in every profile beside them (`~/.claude-work`,
  `~/.codex-personal`). `CLAUDE_CONFIG_DIR` and `CODEX_HOME` replace all of them with one root.
  pi from `~/.pi/agent/sessions` (`PI_CODING_AGENT_DIR` moves the agent directory) and an absolute
  `sessionDir` from its `settings.json`; `PI_CODING_AGENT_SESSION_DIR` replaces both. A forked pi
  session counts only what it added to the copy of its source. fx from `~/.fx/sessions`
  (`FX_HOME`, which only cctrail reads, moves `~/.fx`); fx logs a session's tokens only as totals,
  so they are spread over its agents' activity, a share on every active day: exact per project,
  approximate per day.
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
  `gpt-6-luna`. pi's and fx's models read and are priced alike whatever the provider (Bedrock's
  `us.anthropic.claude-opus-4-6-v1` as `claude-opus-4-6`, a router's `openai/gpt-5.4` as
  `gpt-5.4`, fx's `codex/gpt-6-astra` as `gpt-6-astra`), at the harness's own estimate for a model
  models.dev lacks; an fx session in Fast mode counts at the fast rate throughout. Tokens on other
  unpriced models are listed below the report.

## 🤝 Contributing

Pull requests are welcome, and so are your agents: point them at [agents.md](agents.md).

<details>
<summary>Installing a branch</summary>

```sh
cargo install --git https://github.com/vladkens/cctrail --branch feat/NAME --locked  # a PR branch
cargo install --git https://github.com/vladkens/cctrail --locked --force             # back to main
cargo install --path . --locked                                                      # a local checkout
cargo uninstall cctrail                                                              # remove it
```

</details>

## 🙏 Credits

- [models.dev](https://github.com/anomalyco/models.dev) (MIT): the model prices, read from its git
  history for the price of each day.
- [ratatui](https://github.com/ratatui/ratatui) (MIT): the interactive view.

## 📝 License

Distributed under the [MIT License](LICENSE).

## 🔍 See also

- [ccusage/ccusage](https://github.com/ccusage/ccusage) – Daily, weekly, monthly and session
  reports of tokens and cost from the local data of many coding agent CLIs.
- [Maciek-roboblog/Claude-Code-Usage-Monitor](https://github.com/Maciek-roboblog/Claude-Code-Usage-Monitor)
  – A live terminal monitor of Claude Code usage against its rate limits, with forecasts.
