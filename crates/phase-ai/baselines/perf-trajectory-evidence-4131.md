# Exploratory trajectory notes (#4131) — NOT baseline commit evidence

**Status:** Hypothesis-generation only. Does **not** satisfy
`scripts/refresh-ai-perf-baseline.sh` or `scripts/validate-ai-perf-reproducibility.sh`.
Do **not** use this file to justify changing `perf-baseline.json`.

This PR reverted an premature baseline refresh and leaves `perf-baseline.json`
matching `origin/main`. The decision-cost perf gate **will fail** on this branch
until a **separate** shared baseline repair lands on `main` with retained
margin/reproducibility evidence (median-of-5 diff + validate script output
committed or attached to that repair PR).

## Exploratory question

Does the life-auction branch increase decision cost on gate scenarios
(`red-mirror`, `affinity-mirror`, `enchantress-mirror`), or does CI fail because
card-data regeneration shifted trajectories relative to a stale baseline?

## Method (incomplete — local only)

Fixed workload: seed `0x9E37_79B9`, action cap `3000`, three mirror scenarios.

| Run | Engine | Card-data | Sample quality |
|-----|--------|-----------|----------------|
| A | committed baseline | hash `4bf63d…` | committed baseline |
| B | `origin/main` worktree | main-parser regen `dc3c022…` | **single cold sample** |
| C | #4131 branch | main-parser regen `dc3c022…` | **single cold sample** |
| D | #4131 branch | feature-parser regen `4f309e…` | **single cold sample** |
| E | #4131 branch | feature-parser regen `4f309e…` | median-of-5 — see `perf-gate-pre-refresh-4131.log` |

Runs B–D are not statistically validated. Run E is retained in-repo as the
pre-refresh median-of-5 diff only; it does **not** authorize a baseline change
without a completed `validate-ai-perf-reproducibility.sh` margin report.

## Single-sample counter snapshot (B/C/D)

| Counter | A baseline | B main eng | C branch eng | D branch feat | C−B | D−C |
|---------|----------:|-----------:|-------------:|--------------:|----:|----:|
| `state_clone_for_legality` | 4,827 | 12,222 | 12,203 | 12,221 | −19 | +18 |
| `mana_aura_trigger_scans` | 10,525 | 20,954 | 20,932 | 20,948 | −22 | +16 |

These deltas suggest engine/parser isolates are noise-scale on paths mirror
scenarios exercise, while regenerating card-data on `origin/main` alone reproduces
most of the CI shift. That remains **unverified** until the baseline owner runs
the full refresh workflow below.

## What is required to change the protected baseline (out of scope for #4131)

1. Publish median-of-5 `cargo ai-perf-gate` diff (`perf-gate-pre-refresh-4131.log`).
2. Run `scripts/validate-ai-perf-reproducibility.sh` to completion without
   mutating `perf-baseline.json` mid-run; attach margin report.
3. Land baseline repair on `main` via `scripts/refresh-ai-perf-baseline.sh`.
4. Rebase #4131 onto that `main` head and re-run CI.
