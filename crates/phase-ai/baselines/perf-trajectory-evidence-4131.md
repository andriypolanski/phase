# Decision-cost perf gate — trajectory evidence (#4131)

This documents the **controlled experiment** required before refreshing
`perf-baseline.json`. The premature baseline rewrite on an earlier head was
**reverted**; `perf-baseline.json` matches `origin/main` until this evidence is
reviewed and `scripts/validate-ai-perf-reproducibility.sh` passes.

## Question

Does the life-auction branch increase decision cost on the three gate scenarios
(`red-mirror`, `affinity-mirror`, `enchantress-mirror`), or does CI fail only
because card-data regeneration shifted AI trajectories?

## Method

Fixed workload: seed `0x9E37_79B9`, action cap `3000`, three mirror scenarios.

| Run | Engine | Card-data | Sample |
|-----|--------|-----------|--------|
| A | `origin/main` @ baseline commit | baseline hash `4bf63d…` | committed baseline |
| B | `origin/main` (worktree build) | main-parser regen `dc3c022…` | single cold sample |
| C | **#4131 branch** | main-parser regen `dc3c022…` | single cold sample |
| D | **#4131 branch** | feature-parser regen `4f309e…` | single cold sample |
| E | **#4131 branch** | feature-parser regen | median-of-5 (`cargo ai-perf-gate`) |

Regenerated card-data uses the same MTGJSON snapshot (`data/mtgjson/` symlink).
Main-parser export: `origin/main` worktree `oracle-gen`. Feature-parser export:
branch `client/public/card-data.json` → `data/card-data.json`.

## Key counters (single-sample B/C/D)

| Counter | A baseline | B main eng | C branch eng | D branch feat | C−B | D−C |
|---------|----------:|-----------:|-------------:|--------------:|----:|----:|
| `state_clone_for_legality` | 4,827 | 12,222 | 12,203 | 12,221 | −19 | +18 |
| `mana_aura_trigger_scans` | 10,525 | 20,954 | 20,932 | 20,948 | −22 | +16 |
| `restriction_static_mode_gate_scans` | 29,286 | 42,750 | 42,685 | 42,756 | −65 | +71 |
| `layers_incremental` | 198 | 307 | 307 | 307 | 0 | 0 |
| `auto_tap_source_cache_builds` | 210 | 399 | 388 | 396 | −11 | +8 |
| `layers_full_eval` | 6,533 | 5,485 | 5,475 | 5,488 | −10 | +13 |
| `attackable_player_sweeps` | 727 | 500 | 488 | 504 | −12 | +16 |

Median-of-5 (E) vs baseline (A): **8 FAIL**, same eight counters CI reported;
see `/tmp/perf-gate-vs-main-baseline.log` on the analysis machine.

## Conclusions

1. **Engine isolate (C vs B):** Branch life-auction engine delta is **noise-scale**
   (±0.2% on `state_clone_for_legality`). The auction prompt paths are not
   exercised in mirror scenarios.

2. **Parser isolate (D vs C):** Feature-parser card-data delta is **noise-scale**
   vs main-parser on the same engine. The Illicit/Pain's/Mages' parser block is
   not driving the gate counters.

3. **Card-data era (B vs A):** Regenerating card-data from the **main** parser
   with current MTGJSON already reproduces the ~2.5× `state_clone_for_legality`
   and ~2× `mana_aura_trigger_scans` shift. The committed baseline hash
   (`4bf63d…`) is stale relative to any fresh export (`dc3c022…` main parser,
   `4f309e…` feature parser).

4. **Bidirectional counter movement** under E (vs A): several counters **decrease**
   (`layers_full_eval` −1037, `mana_display_swept_objects` −2401,
   `attackable_player_sweeps` −213) while others increase — inconsistent with a
   uniform per-probe regression in shared legal-action code.

## Required follow-up before baseline refresh

1. Paste the median-of-5 diff from `cargo ai-perf-gate` into the PR (log above).
2. Run `scripts/validate-ai-perf-reproducibility.sh`; attach margin report.
3. Only then: `scripts/refresh-ai-perf-baseline.sh` and commit the new baseline
   with this document linked.

## Raw sample paths (local analysis)

- `/tmp/perf-main-engine-main-parser-data.json`
- `/tmp/perf-branch-main-parser-data.json`
- `/tmp/perf-branch-feature-parser-data.json`
- `/tmp/perf-gate-vs-main-baseline.log` (median-of-5 vs main baseline)
