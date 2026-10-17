# Performance

Every speed attempt, kept or not: what was tried, the result (`make bench`, warm cache), and why it
was kept or reverted. Newest last.

Baseline: Python cctrail 0.2.1, `cctrail stats --by workdir --json` on ~10 GB of Codex and ~0.9 GB
of Claude history (M-series Mac, 2026-10-14): 27.2 s wall, 18.0 s user, 528 MB peak RSS.

Rust port (feat/rust-port, same history, `make bench-ref`, 2026-10-14): 4.4 s wall (4.1–4.8),
7.7 s user, 3.1 s system, ~570 MB peak RSS; 0.2.1 in the same run 26.5 s. Only ~2.5 cores busy and
memory above the Python's: the leads for phase 2.

Caching repeated pure work (feat/memo, 2026-10-14): the local day of a moment by its span (one
offset, one day), folds per distinct directory, `here`'s project per directory pair, billable
events once per command instead of twice. One run each, `--by dir --json`: 4.82 → 4.55 s wall,
7.41 → 7.63 s user, 541 → 485 MB peak RSS: within noise, parsing dominates. Kept: no cost, less
memory, and the work no longer grows with events × lookups.

Swearing in prompts for the interactive view (feat/swears, 2026-10-17): the parsers read a
prompt only when asked, parsing its raw line again and keeping its counts of words and swear words,
not its text; the text reports parse as before. `--by dir --json` A/B/A, 5 runs each: main 4.48 s
and 4.42 s, the branch 4.53 s, user time 7.50 / 7.41 / 7.32 s: unchanged. Codex logs prompts since
2026-08 as `item_completed` items, whose other kinds hold commands' output: parsing those lines as
`serde_json::Value` cost 1.35 s user on one month of Codex history (1.4 GB) against 1.03 s for the
report; a struct with only the fields read, 1.15 s. Kept the struct. The interactive view's load,
one run each in one process: 4.54 s to parse without prompts and 0.05 s for its data, 4.36 s and
0.02 s with them: within noise.

The ledger (feat/ledger, 2026-10-17): the parsed files in SQLite, read again only when their size
or mtime changes. `--by dir --json`, one run each: the logs alone (`CCTRAIL_NO_LEDGER=1`) 4.69 s
wall, 7.40 s user, 573 MB peak RSS; the first run, building the ledger (prompts read too, 510k
events written in one transaction) 5.41 s, 10.46 s user, 598 MB; the next runs with nothing but
the running session changed 0.43 s and 0.42 s, 0.37 s user, 310 MB. The database is 42 MB for
4.3k files and 510k events (11.8 GB of logs), models and efforts interned. Without the ledger the
parsers now read every copy of a file before dropping the extra ones: A/B/A/B against main 3.98 /
4.38 / 4.29 / 4.16 s wall, 7.30 / 7.35 / 7.20 / 7.36 s user, within noise. A clean release build
takes 39.3 s instead of 24.2 s (158 s CPU instead of 130 s): bundled SQLite's C, 25 s, is the
longest unit; the binary grows from 6.6 to 9.1 MB. Kept: `make compare` identical cold and warm.
