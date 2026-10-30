# cctrail

A small Rust CLI that reads local Claude Code, Codex, pi and fx history and reports active time, tokens and estimated cost per project. Its one user is the author: keep the CLI minimal and the numbers right. The CLI is `cctrail --help`, config and history locations are in the readme, prices in `src/pricing.tsv` (built by `scripts/pricing-history.py`, published daily by `pricing.yml`), phases in [docs/roadmap.md](docs/roadmap.md), speed attempts in [docs/perf.md](docs/perf.md).

## Boundaries

- Only read history: never write into `~/.claude*`, `~/.codex*`, `~/.pi`, `~/.fx` or any other history directory it reads (the environment overrides, pi's `sessionDir`). cctrail writes only its cache, the file `cctrail export` names and its data directory on `cctrail import`, and uses the network only for the price catalog (`CCTRAIL_NO_UPDATE=1` turns that off; tests set it).
- Change the CLI, the JSON output (it carries `schema_version`) or any number cctrail reports only with the person's approval, and say in the PR body which rule moved what.
- The repo is public: never put the person's own numbers (costs, tokens, hours) or private project names in PR bodies, review replies, commits or docs.
- Add a harness only on request, as its own `src/parser/<harness>.rs`.
- Keep the build small: prefer an own routine to a dependency that compiles far more than cctrail uses; no clap, table or color crates; a heavy dependency only with the person's approval. Change dependencies only with `cargo add` / `cargo remove` / `cargo upgrade`, never by hand.
- Support macOS, Linux and Windows.

## Code

- Price an event at its day's rate, never today's.
- History is untrusted input: skip what doesn't parse, never panic or fail the run on it.
- Keep it within ~8000 lines of Rust without tests and each feature a few hundred lines; simplify before growing.
- Comment only a harness's format quirks (with the version that introduced them) and non-obvious accounting rules.
- Start every `.rs` file with `// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail`.
- Unit tests go inline in `#[cfg(test)] mod tests`; `tests/` holds end-to-end CLI tests on `tests/fixtures/`: small hand-written synthetic JSONL, never real history.
- Helper scripts go in `scripts/`: `sh`, or Python run with `uv run`.

## Checks

- `make check` and `make test` pass before every commit.
- When parsing or accounting changes, `make compare REF=<main's binary>` must match on the local history: tokens, sessions and first/last activity exact, costs within 1e-9, active time within a second per group.
- Measure speed with `make bench` A/B/A, rerunning when the two A runs differ by over 5%; log every attempt in `docs/perf.md` and keep a change only when `make compare` matches and the gain is clear.

## Workflow

- Commits and PR titles are one short lowercase line with a conventional prefix (`feat:`, `fix:`, `docs:`, `ci:`, `chore:`), no body; PRs are squash-merged.
- Multi-step work follows a checklist in `docs/plans/yyyymmdd-<name>.md`, agreed with the person, with only its boxes ticked during the work. The PR that ticks the last box adds `Done: yyyy-mm-dd, PR #N` as the first line and a short `## Result`, and moves it to `docs/plans/completed/`; never delete a plan.
- Docs are only `readme.md`, `docs/perf.md`, `docs/roadmap.md` and `docs/plans/`. `changelog.md` changes only in the person's `release: vX.Y.Z` PR, which also bumps the version and runs `make update` for fresh prices.
