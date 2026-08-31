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
