# Roadmap

cctrail answers one question: how much time and how many tokens each project takes, across Claude
Code and Codex. Tick a phase when done.

- [x] **1. Rust port:** a minimal CLI with a warning for unpriced models, its numbers checked on
      the full local history (#1). [Plan](plans/completed/20261014-rust-port.md)
- [ ] **2. Fast:** the full history (~11 GB) well under the 27 s baseline; log attempts in
      [perf.md](perf.md).
- [ ] **3. Release v0.3.0** on crates.io and as GitHub release binaries (macOS, Linux, Windows).
      0.0.0 holds the name on crates.io; `main.yml` publishes through Trusted Publishing.
- [x] **4. Dated prices:** every request at its day's price from the models.dev git history,
      refreshed daily without the person's help (#3).
      [Plan](plans/completed/20261014-price-history.md)
- [x] **5. Workdirs:** agent worktrees fold into their repository (#4).
      [Plan](plans/completed/20261014-workdirs.md)
- [x] **6. Hybrid stats:** unconfigured repositories beside the projects, the small tail in one
      row (#6). [Plan](plans/completed/20261014-hybrid-stats.md)
- [x] **7. Clones by name and --all:** same-named directories fold into the one used last; `--all`
      lists every row (#7). [Plan](plans/completed/20261014-clones.md)
- [x] **8. TUI:** `cctrail` on a terminal opens projects, models and a timeline with drill-downs
      (#10). [Plan](plans/completed/20261015-tui.md)

Later, only on request: a cache of parsed sessions keyed by file size and mtime, so a run reads
only new history; other harnesses (Gemini CLI, opencode).
