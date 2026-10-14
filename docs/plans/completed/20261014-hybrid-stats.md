Done: 2026-10-14, PR #6

# Hybrid stats

`cctrail` hides every unconfigured repository in one `@unmapped` row until each is configured by
hand, and `--by dir` lists every directory, most of them a minute in a temp folder. Make
the default report useful without a config: configured projects and unconfigured repositories in
one table, the small tail in one row.

## Decisions

- One table sorted by time: configured projects by their label, unconfigured folded directories
  by their path (`~/Code/app`), dimmed. No `@unmapped` row.
- Rows sorted by time are listed until they cover 95% of the period's time; the unconfigured rest
  collapses into one `other (N dirs)` row above `total`, at most 5% of the time. Configured
  projects always keep their row. `--by dir` folds the same way; `--by group` doesn't.
- Below the table: `N small dirs in other · run cctrail --json for all` (`--by dir --json` by
  dir), then the config path or setup hint as today.
- JSON lists every directory, unconfigured ones under group `@unmapped`, as before.
- Durations in tables: `8h 14m`, and from a day on `45d 8h 14m`.
- Numbers: totals unchanged; only the text layout of `cctrail` / `--by project` changes.
- In the same PR: `~/.claude-*` and `~/.codex-*` profiles are read by default; `codex-auto-review`
  (no official price) is billed as `gpt-6-luna`; repeated work is cached (local days, folds,
  billable events).

## Tasks

- [x] Text report: hybrid rows, dimmed unconfigured labels, the `other` row and its hint; unit
      tests for the threshold (relative to the period total, configured rows never collapse, no
      `other` when nothing is small) and end-to-end tests on the fixtures.
- [x] Readme ("Usage", "Projects": the config is optional, for naming and merging), roadmap
      entry.

## Result

Totals unchanged. Without a config the report lists repositories by time with `other` at
most 5% of it; profile history and Codex auto-review usage now count.
