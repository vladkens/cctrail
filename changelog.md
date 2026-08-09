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
