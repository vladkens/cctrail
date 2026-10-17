## v0.4.0 – 2026-10-18

### Features

- Added a Swears view to the interactive view: how often you scold each model, adjusted for how much you used it (#15)
- Added `cctrail export FILE` and `cctrail import FILE` to move the history to another machine, or keep it after the harnesses delete their logs (#19)
- Added cost per active hour and per million tokens to the Models view (#16)

### Improvements

- Improved the Models view: one row per model whichever harness ran it, with a per-harness breakdown inside a model, dated model ids merged, sorted by cost by default (#16)

### Fixes

- Fixed time of Claude Code background agents and fx agents counted under an unknown model in the interactive view (#17)
- Fixed Claude Code's interruption and error messages taking model time in the interactive view (#16)

**Full Changelog**: https://github.com/vladkens/cctrail/compare/v0.3.0...v0.4.0

---

## v0.3.0 – 2026-10-17

### Breaking Changes

- Rewrote cctrail in Rust: install it with `cargo install cctrail` or a prebuilt binary; the Python package is no longer updated (#1)
- Simplified the CLI: statistics are the default command (`cctrail [period] [--by project|group|dir]`), `here` shows the current project, `days`, `weeks` and `months` show periods; `--no-config` and `--show-unmapped` are replaced by `--by dir` and `--all` (#1)

### Features

- Added an interactive view, opened by `cctrail` on a terminal: projects, models and a timeline stacked by model, with drill-downs into a project's models, a model's speed and effort, and a day's projects, keyboard and mouse (#10)
- Added pi and fx history, with `--provider pi` and `--provider fx` (#11, #12)
- Added reading of every Claude Code and Codex profile beside the default one (`~/.claude-*`, `~/.codex-*`) (#6)
- Added folding of agent worktrees and same-named clones into their repository, with no config (#4, #7)
- Added unconfigured repositories as their own rows beside the configured projects, and `--all` to list every row (#6, #7)
- Added prices by date: each request is priced at its day's rate, and prices refresh in the background once a day (#3)

### Improvements

- Improved speed about six times on a large history: about 4.4 s instead of 26.5 s for ~11 GB (#1)
- Added prebuilt binaries for macOS, Linux and Windows (#2)

**Full Changelog**: https://github.com/vladkens/cctrail/compare/v0.2.1...v0.3.0

---

## v0.2.1 – 2026-09-01

### Features

- Added a current-project activity dashboard with today, yesterday, weekly, monthly, and all-time summaries
- Added `stats --show-unmapped` to reveal individual unmapped working directories while keeping the default report compact

### Fixes

- Fixed tables overflowing and wrapping incorrectly in narrow terminals

### Improvements

- Added the `-V` shorthand for displaying the installed version
- Added contextual project-config paths and setup guidance to `stats` output and help

**Full Changelog**: https://github.com/vladkens/cctrail/compare/v0.2.0...v0.2.1

---

## v0.2.0 – 2026-08-29

### Breaking Changes

- Replaced the `projects`, `groups`, and `audit` commands with the unified `stats` command and removed configurable history sources, project aliases, legacy command options, and the Python library API

### Features

- Added project, group, and working-directory statistics with strict `<group>/<project>` mapping and a separate `@unmapped` section
- Added compact JSON reports, model breakdowns, Monday-based weeks, clipped custom date ranges, and the global `--no-config` option
- Added `update-pricing` to download current model prices into a persistent user cache

### Improvements

- Improved Claude usage deduplication and subagent handling, including fallback usage from tool results
- Improved Codex spawned-agent and forked-rollout handling to avoid replayed token usage and preserve project attribution
- Excluded failed events from billing and active-time calculations

**Full Changelog**: https://github.com/vladkens/cctrail/compare/v0.1.1...v0.2.0

---

## v0.1.1 – 2026-08-10

### Fixes

- Fixed active-time inflation caused by sessions without usage

### Maintenance

- Completed PyPI package metadata with project links, classifiers, and discovery keywords

### Docs

- Documented how Claude Code transcript retention affects historical reports

**Full Changelog**: https://github.com/vladkens/cctrail/compare/v0.1.0...v0.1.1

---

## v0.1.0 – 2026-08-09

### Features

- Added Claude Code and Codex history parsing with nested agent support
- Added usage, project, group, and pricing audit reports with calendar periods, provider filters, model breakdowns, and JSON output
- Added estimated costs and active time with overlapping activity distributed across projects and groups
- Added project aliases, configurable groups, history source directories, and config selection options
- Added a Python API for parsed sessions, metrics, pricing, and serialization

### Improvements

- Improved history parsing performance with parallel processing and cached project matching

**Full Changelog**: https://github.com/vladkens/cctrail/commits/v0.1.0
