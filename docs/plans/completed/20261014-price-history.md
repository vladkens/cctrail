Done: 2026-10-14, PR #3

# Price history

A cost must not change when the history is recounted later. Today `src/pricing.json` holds one
snapshot of models.dev: a price cut reprices all past usage, and a model models.dev removed costs
$0 from then on. Price every event at the rate in force on its day instead, from a dated catalog
built from the models.dev git history, and keep that catalog fresh without the person running
anything.

## Decisions

- Source: the git history of github.com/anomalyco/models.dev, `providers/{anthropic,openai}/models/
  <model>.toml` (`[cost]`, `[[cost.tiers]]`, `[experimental.modes.fast].cost`). A price takes effect
  on the date of the commit that set it (committer date, UTC). models.dev lags real changes by a
  few days (gpt-5.6-sol: cut on 2026-08-22, committed 2026-08-25); that is accepted.
- Corrections: an edit within 14 days of the model's first commit replaces the earlier price for
  all dates (a data fix, not a price change). Later edits start a new period.
- The first known price also applies before its date; a model removed from models.dev keeps its
  last price.
- Fast mode: price it from the fast `cost` table when present (per field), else from the standard
  one; drop `fast_multiplier` if the table covers it.
- Format: `src/pricing.tsv`, a header and one row per price period (provider, model, since,
  prices; an empty cell for a price not set), sorted by provider, model and date. The bundled file
  is the fallback; a downloaded one in `~/.cache/cctrail` wins.
- Delivery: a weekly scheduled workflow (`.github/workflows/pricing.yml`, plus
  `workflow_dispatch`) rebuilds the catalog and uploads it as `pricing.tsv` to a rolling
  `pricing` GitHub release (created if missing, asset replaced). It needs `contents: write` only
  in that job and runs no third-party code: it reads TOML from the models.dev checkout.
- Auto-update (approved by the person): when the downloaded catalog is missing or older than 7
  days, cctrail fetches the release asset in the background while it parses the history (one
  request, a few seconds' timeout), uses it if it arrives in time and is valid, and saves it.
  Failures are silent. `CCTRAIL_NO_UPDATE=1` turns it off; tests set it. `cctrail update-pricing`
  fetches the same asset on demand and reports errors. agents.md's network rule changes to say
  this.
- `make update` rebuilds `src/pricing.tsv` locally from a models.dev checkout in `3rd/`
  (gitignored) with the same script.

## Tasks

- [x] `scripts/pricing-history.py` (`uv run`): walks the models.dev git log for the two providers,
      parses each version of each model file, applies the correction rule, writes the dated
      catalog. Unit-test the rules on a tiny synthetic git repo or on parsed inputs.
- [x] Dated catalog in Rust: parse the list format, pick the entry for an event's local date, fast
      table; unit tests for before-first, between, after-last, removed model, correction.
      Regenerate `src/pricing.tsv`.
- [x] Auto-update and `update-pricing` from the `pricing` release asset; `CCTRAIL_NO_UPDATE`;
      the unpriced warning's update hint adapts; the end-to-end tests never touch the network.
- [x] `.github/workflows/pricing.yml` and `make update`; run the workflow once by
      `workflow_dispatch` from the branch to prove it publishes the asset.
- [x] Readme ("How it counts"), agents.md network rule, roadmap entry.
- [x] Numbers: tokens, sessions and active time identical to `main` on the full history; the PR
      body lists the cost per month before and after.

## Result

Tokens, sessions and active time identical to `main` on the full history. Costs moved only where
a price changed after a model's first price: GPT-5.6 sol, terra and luna requests before
models.dev recorded the 2026-08-25 cut (sol $5/$30 → $4/$20 per million) now use the older,
higher rate, so July and August cost more; gpt-5.4 requests over its long-context threshold in
April–May now get the long-context rate from the older catalog layouts. Other months unchanged.
