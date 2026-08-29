## v0.2.0 – 2026-08-29

### Features

- Reworked history parsing around deduplicated usage and activity events, including Claude subagents and Codex spawned or forked rollouts
- Added the unified `stats` command with project, group, and working-directory views
- Added strict project mapping through optional `<group>/<project>` configuration
- Added `--no-config`, model breakdowns, compact JSON reports, Monday-based weeks, and clipped custom date ranges

### Changes

- Simplified the CLI to `usage` and `stats`; removed the old project, group, audit, source, alias, and Python library interfaces
- Kept distribution as a regular Python package for `uvx`, `uv tool install`, and PyPI

**[Full Changelog](https://github.com/vladkens/cctrail/compare/v0.1.1...v0.2.0)**

---

## v0.1.1 – 2026-08-10

### Fixes

- Fixed active-time inflation caused by sessions without token usage

### Maintenance

- Completed PyPI package metadata with project links, classifiers, and discovery keywords

**Full Changelog**: https://github.com/vladkens/cctrail/compare/v0.1.0...v0.1.1

---

## v0.1.0 – 2026-08-09

### Features

- Added parsing for local Claude Code and Codex histories, including messages, tool calls, and nested agents
- Added usage, project, group, and pricing audit reports with calendar periods, provider filters, model breakdowns, and JSON output
- Added estimated costs and active time, with overlapping activity distributed across projects and groups
- Added optional TOML configuration for project aliases, project groups, and history source directories
- Added configuration selection and opt-out options for individual commands
- Added a Python API for parsed sessions, metrics, pricing, and model serialization

### Improvements

- Improved performance through parallel history parsing and cached project matching

**Full Changelog**: https://github.com/vladkens/cctrail/commits/v0.1.0
