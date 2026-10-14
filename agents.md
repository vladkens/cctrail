# cctrail

A small Rust CLI that reads local Claude Code and Codex history and reports, per project, active
time, tokens and estimated cost. Its one user is the author: keep the CLI minimal and the numbers
right. Agents carry out each change end to end by following this file; plan in
[docs/roadmap.md](docs/roadmap.md).

Each fact has one source, which this file points to: the CLI is `cctrail --help`, the config format
and history locations the readme, prices `src/pricing.json`, dependencies `Cargo.toml`, checks and
CI the `Makefile` and `.github/workflows/main.yml`.

## Scope

- Read history only: never write into `~/.claude` or `~/.codex`. cctrail writes only under
  `~/.cache/cctrail`, and uses the network only to fetch the price catalog from the `pricing`
  release: on `cctrail update-pricing`, and in the background when the cached one is a week old
  (`CCTRAIL_NO_UPDATE=1` turns that off; tests set it).
- Prices are dated: `scripts/pricing-history.py` builds them from the models.dev git history, the
  weekly `pricing.yml` workflow publishes them. Price an event at its day's rate, never at today's.
- Support Claude Code (`~/.claude/projects`, `CLAUDE_CONFIG_DIR`) and Codex (`~/.codex/sessions`
  and `archived_sessions`, `CODEX_HOME`). Add another harness only on request, as its own
  `src/parser/<harness>.rs`.
- Keep the build fast: prefer a small own routine to a generic dependency that compiles far more
  code than cctrail uses (`cargo build --timings`). The dependencies are serde/serde_json (history
  and JSON output), toml (config), jiff (local time zones), rayon (parallel parsing), anyhow, ureq
  (`update-pricing`) and libc (terminal width). No clap, table or color crates: arguments, tables
  and ANSI colors are hand-rolled. Change dependencies only with `cargo add` / `cargo remove` /
  `cargo upgrade`, never by hand.
- The CLI is what `cctrail --help` shows and the JSON output carries `schema_version`: change
  either only with the person's approval.
- Run on macOS, Linux and Windows.

## Code

- One binary crate, flat modules: `cli` (arguments, commands), `config`, `model`,
  `parser/{claude,codex}`, `metrics`, `output` (tables, JSON), `pricing`. Keep it within ~3000
  lines of Rust, tests excluded; simplify before growing past it.
- Treat history as untrusted input: harnesses change formats and leave truncated lines. Skip what
  doesn't parse, never panic or fail the run on it.
- Errors go through `anyhow` with the path or value that failed. Comment only format quirks of a
  harness (name the version that introduced them) and non-obvious accounting rules.
- Start every `.rs` file with the line
  `// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail`.
- Put helper scripts in `scripts/`, named in kebab-case: `sh`, or Python run with `uv run`. A
  Makefile target longer than a line or two calls a script.

## Correctness

- The numbers are the product: tokens, sessions, active seconds and first/last activity must not
  drift between versions. Treat any change in them as a bug unless it is a fix the person approved;
  say in the PR body where it changes and why (which model, price, date or directory rule).
- The repository is public: never put the person's own numbers (costs, tokens, hours, history
  size) or private project names in PR bodies, review replies, commits or docs. Fixture numbers,
  speed measurements and "identical to `main`" are fine; report the rest to the person directly.
- Until the port is done, the reference is the Python cctrail 0.2.1 (`uvx cctrail==0.2.1`).
  `make compare` runs both on the local history and diffs the JSON: tokens, sessions, seconds and
  timestamps exact, costs within 1e-9.
- Unit-test logic that can break silently, inline in `#[cfg(test)] mod tests`: deduplication,
  subagent linking, fork replay, activity windows and their split by day and between projects, date
  ranges, config mapping, pricing. `tests/` holds end-to-end CLI tests on `tests/fixtures/`.
- Fixtures are small synthetic JSONL written by hand. Never commit real history or excerpts of it:
  it holds private prompts, code and paths. When a harness version changes its format, reproduce
  the change in a minimal fixture first.

## Performance

- Every command reads the full local history, gigabytes of logs. Measure with `make bench` (hyperfine, warm cache), A/B/A back to back; when the two A runs
  differ by more than 5%, rerun later instead of concluding.
- Log every speed attempt in [docs/perf.md](docs/perf.md): what was tried, the result, kept or
  reverted and why. Keep a change only when `make compare` still matches and the gain is clear.
  Delete rejected code.

## Workflow

- Before committing, run `make check` and `make test`. `make prepare` rewrites files: not a check.
- Every change reaches `main` through a pull request, one per feature or roadmap phase:
  1. Agree the plan with the person. For multi-step work keep a checklist in
     `docs/plans/yyyymmdd-<name>.md` and during the work only tick its checkboxes, with no progress
     prose. The PR that finishes the plan's last item, in its own commits, adds a first line
     `Done: yyyy-mm-dd, PR #N` and a short final `## Result` section with the key results, and
     moves it to `docs/plans/completed/` (fixing links to it). Plans stay; never delete them.
  2. Branch from `main` as `feat/<name>`, in the agent's own git worktree under
     `.claude/worktrees/`. A small change that fits an open PR's topic goes into that PR instead of
     a new one: docs-only edits join the open docs PR.
  3. Commit every step that passes `make check` and `make test`: one short lowercase line, no
     body, docs updated in the same commit. The branch is squashed.
  4. Push and open the PR with `gh pr create`. The title is the squash commit, one short lowercase
     line; the body says what changed, the checks run, `make compare` and numbers for speed work.
  5. The orchestrating agent reviews the diff, reruns the checks and sends findings back until the
     PR is clean, then hands it to the person. Copilot reviews every PR: fix what is right, answer
     the rest in the thread and resolve it. Once the orchestrator accepts the solution, the
     implementing agent runs `codex -C <worktree> review -c model_reasoning_effort=xhigh "Review
     the changes on the current branch against origin/main (git diff origin/main...HEAD)."` if
     available and fixes or rejects each finding with a reason; the orchestrator checks that last
     diff before handing the PR over.
  6. The person does the final review and squash-merges. Never commit or push to `main` directly.
  7. On conflicts, rebase onto `main`, rerun the checks and push with `--force-with-lease`.
- A release goes through a PR too: a branch with one `release: vX.Y.Z` commit (changelog written
  by the person's skill, version bumped, `make update` run for fresh prices), squash-merged; CI
  then tags it, publishes to crates.io and creates the GitHub release with the binaries.
- An orchestrating agent writes each task, verifies every report itself (`make check`, `make test`,
  `make compare`, the diff) instead of trusting it, and keeps the person informed.
- Report a blocker as soon as you hit it: a missing permission or tool, a failing service, or
  anything else that stops the task. Tell whoever delegated the task right away (a subagent
  messages the orchestrator), say what can still be done without it, and keep doing that part.
  Never wait or poll silently for more than a few minutes.
- Ask the person only when the numbers or the CLI must change, a heavy dependency is needed, or the
  request is ambiguous. Otherwise decide and say so in your report.
- Keep docs to `readme.md` (users), `docs/perf.md`, `docs/roadmap.md` and `docs/plans/`. No
  progress journals, run logs or disclaimer paragraphs. `changelog.md` is written only in a release
  commit, by the person's changelog skill from the git history: don't edit it in feature work.
