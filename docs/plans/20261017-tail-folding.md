Status: draft, first thoughts; not agreed for implementation.

# Tail folding

Views that list every row (the interactive view, `--all`) end with many tiny rows: one-off
`codex exec` sessions that a script started in throwaway folders, benchmark runs in temp dirs. They
are real sessions, but each folder becomes its own row.

## Thoughts

- Temp dirs (`/tmp`, `/private/tmp`, `/var/folders`, `$TMPDIR`) fold into one `temp` row.
- A directory with no git info in any of its sessions folds into its highest ancestor below a
  container (home, `~/Downloads`, `~/Desktop`, `~/Documents`, `~/Code`), but only when two or more
  such rows meet there, and never into a git repository.
- Tokens, sessions and time don't change, only the grouping; `--by dir` keeps every directory.
- Same-named clone folding should not merge such no-git siblings with each other.

## Open questions

- Which containers to know by default, and whether a config can add more.
- Whether `temp` is one row or one per kind of temp dir.
