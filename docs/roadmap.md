# Roadmap

cctrail answers one question: how much time and how many tokens each project takes, across Claude
Code, Codex, pi and fx. Tick a phase when done.

- [x] **1. Rust port:** a minimal CLI with a warning for unpriced models, its numbers checked on
      the full local history (#1). [Plan](plans/completed/20261014-rust-port.md)
- [ ] **2. Fast:** the full history (~11 GB) well under the 27 s baseline; log attempts in
      [perf.md](perf.md).
- [x] **3. Release v0.3.0** on crates.io and as GitHub release binaries (macOS, Linux, Windows),
      published by `main.yml` through Trusted Publishing (#13).
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
- [x] **9. pi and fx:** two more harnesses beside Claude Code and Codex (#11, #12).

Drafts, first thoughts only, not agreed for implementation:

- [Swears per model](plans/20261017-swears.md): how often the user swears at each model.
- [Ledger](plans/20261017-ledger.md): parsed history kept by cctrail, so it outlives the logs,
  moves between machines and a run reads only new history.
- [Tail folding](plans/20261017-tail-folding.md): temp dirs and no-git folders in fewer rows.
- [Projects in the interactive view](plans/20261017-tui-projects.md): merge, rename and group
  projects without editing the config.
- [OpenCode](plans/20261017-opencode.md) as another harness.
