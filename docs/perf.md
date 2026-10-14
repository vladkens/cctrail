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
