# cctrail

<div align="center">

[<img src="https://badges.ws/pypi/v/cctrail" alt="version" />](https://pypi.org/project/cctrail)
[<img src="https://badges.ws/pypi/python/cctrail" alt="py versions" />](https://pypi.org/project/cctrail)
[<img src="https://badges.ws/pypi/dm/cctrail" alt="downloads" />](https://pypi.org/project/cctrail)
[<img src="https://badges.ws/github/license/vladkens/cctrail" alt="license" />](https://github.com/vladkens/cctrail/blob/main/LICENSE)
[<img src="https://badges.ws/badge/-/buy%20me%20a%20coffee/ff813f?icon=buymeacoffee&label" alt="donate" />](https://buymeacoffee.com/vladkens)

</div>

`cctrail` is a small CLI that reads local Claude Code and Codex history and reports token usage, estimated cost, active time, and project statistics.

## Install

Run the latest version without installing it:

```sh
uvx cctrail usage
```

Or install the command persistently:

```sh
uv tool install cctrail
```

## Usage

Show usage by local calendar day:

```sh
cctrail usage
cctrail usage today
cctrail usage this-month
cctrail usage --by week
cctrail usage --models
cctrail usage --json
```

Show statistics grouped by configured project, group, or raw working directory:

```sh
cctrail stats
cctrail stats this-month
cctrail stats --by group
cctrail stats --by workdir
cctrail stats --no-config
cctrail stats --json
```

Both commands accept `--provider claude|codex` and custom inclusive date ranges:

```sh
cctrail usage --provider codex --since 2026-08-01 --until 2026-08-31
```

Named periods are `today`, `yesterday`, `this-week`, `last-week`, `this-month`, `last-month`, and `all`. Weeks start on Monday.

### Projects

Projects are configured in `~/.config/cctrail/config.toml`. `XDG_CONFIG_HOME` changes the config root and `CCTRAIL_CONFIG` overrides the complete path.

```toml
[projects]
"env/dotfiles" = [
  "~/Code/dotfiles",
  "~/Code/scripts",
  "~/Code/skills",
]
"oss/cctrail" = ["~/Code/vibe/cctrail"]
```

Each key must have the `<group>/<project>` form. A configured path includes all descendant working directories; when paths overlap, the longest match wins. Unconfigured directories appear under `@unmapped`. Use the global `--no-config` flag before or after `stats` to skip the file and show raw working directories.

Tables use color when stdout is a terminal. JSON always contains unabridged paths and token counts.

Claude history is read from `~/.claude/projects` and Codex history from `~/.codex/sessions` plus `~/.codex/archived_sessions`. `CLAUDE_CONFIG_DIR` and `CODEX_HOME` override those roots.

Claude Code deletes session files older than 30 days at startup by default. To preserve a long-term history, increase [`cleanupPeriodDays`](https://code.claude.com/docs/en/claude-directory#cleaned-up-automatically) before the default cleanup removes those transcripts.

Active time treats every user or assistant event as a five-minute window. Overlapping windows are merged, and simultaneous activity is divided between the names selected by `stats --by`.

Costs are estimates based on the pricing catalog included in the installed package. Run `cctrail update-pricing` to download the latest catalog to `~/.cache/cctrail/pricing.json`; the downloaded catalog takes precedence over the bundled one.

## License

Distributed under the [MIT License](LICENSE).
