# Roadmap

cctrail answers one question: how much time and how many tokens each project takes, across Claude
Code and Codex. Tick a phase when done.

- [ ] **1. Rust port** with the numbers of the Python 0.2.1 on the full local history, a minimal
      CLI, a warning for unpriced models. [Plan](plans/20261014-rust-port.md)
- [ ] **2. Fast:** the full history (~11 GB) well under the Python's 27 s; log attempts in
      [perf.md](perf.md).
- [ ] **3. Release v0.3.0** on crates.io and as GitHub release binaries (macOS, Linux, Windows).
      The person publishes the first version by hand and sets up crates.io Trusted Publishing;
      PyPI stays at 0.2.1.

Later, only on request: a cache of parsed sessions keyed by file size and mtime, so a run reads
only new history; other harnesses (Gemini CLI, opencode).
