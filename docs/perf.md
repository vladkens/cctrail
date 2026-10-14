# Performance

Every speed attempt, kept or not: what was tried, the result (`make bench`, warm cache), and why it
was kept or reverted. Newest last.

Baseline: Python cctrail 0.2.1, `cctrail stats --by workdir --json` on ~10 GB of Codex and ~0.9 GB
of Claude history (M-series Mac, 2026-10-14): 27.2 s wall, 18.0 s user, 528 MB peak RSS.
