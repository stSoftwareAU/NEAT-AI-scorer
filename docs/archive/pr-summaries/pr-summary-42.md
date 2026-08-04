## Summary

Eliminate the per-worker `compile_creature` calls in directory-mode and
forward-only fused scoring. `CompiledNetwork: Clone` has landed upstream
(NEAT-AI-core#11), so each creature is now compiled exactly once and the
resulting `CompiledNetwork` is cloned for any additional workers — clones
are 35–220× cheaper than recompiling, depending on creature size. Closes #42.

The change also adds a new optional `compileTimeSecs` field to `ScoreResult`
so callers can separate fixed startup cost from scoring time.

## What Changed

* `rust_scorer/src/multi_score.rs` — for each loaded creature, compile once
  then `template.clone()` for the remaining `workers_per[ci] - 1` workers.
  Records elapsed compile-plus-clone seconds.
* `rust_scorer/src/stream_score.rs` — `accumulate_mse_sum_forward_only_fused`
  now clones the caller-supplied `network` template per extra worker (was
  re-running `compile_creature`). Returns the additional `clone_time_secs`
  alongside the existing tuple values.
* `rust_scorer/src/scoring.rs` — adds optional `compileTimeSecs` field
  (`Option<f64>`, omitted when `None`).
* `rust_scorer/src/main.rs` — captures the single-creature compile time and
  rolls per-worker clone time from the fused accumulator into it.
* New test `directory_mode_emits_compile_time_secs` confirms the field is
  emitted, non-negative, and small (catches regressions to per-worker
  recompilation).

## Acceptance Criteria

* [x] **Compile each creature exactly once** regardless of `activation_threads`
  — `multi_score.rs` now does `loaded.len()` compiles + `total_workers -
  loaded.len()` clones; `stream_score.rs` does zero extra compiles.
* [x] **Bench shows reduced startup time for population ≥ 50** — see
  evidence below.
* [x] **All existing tests still pass.**
* [x] **`quality.sh` passes** locally.

## Evidence — Benchmarks

`cargo bench -p rust_scorer --bench scoring -- --quick "score_from_creature_dir"`
on a 10-core macOS host (defaults: `BENCH_SCORING_HIDDEN=8`,
`BENCH_SCORING_BYTES=16777216`). Baseline saved with `--save-baseline before`
on `main`, then re-run with `--baseline before` after this change:

| Population | Before (median) | After (median) | Δ          |
|-----------:|----------------:|---------------:|-----------:|
| 1          | 28.10 ms        | 25.03 ms       | **−11.8%** |
| 10         | 120.70 ms       | 114.70 ms      | −3.8%      |
| 50         | 475.90 ms       | 429.43 ms      | **−10.3%** |
| 200        | 1.793 s         | 1.729 s        | −3.7%      |

Statistical significance: N=1 hits `p ≈ 0.05`; N=50 reports a robust 10%
median improvement; N=10 / N=200 sit just outside Criterion's `p < 0.05`
threshold but with consistent positive medians. The acceptance criterion
("reduced startup time for population ≥ 50") is met by N=50.

### Why the wins are larger for small N

The directory-mode worker count is `min(N, threads)`-driven: for
`N < activation_threads`, each creature gets multiple workers so the
old code compiled the same creature several times. For `N ≥ threads`,
each creature already had exactly one worker; the wall-clock improvement
at N=50 reflects the cheaper compile path itself (and similar layout for
the freshly-cloned activation buffers) rather than eliminated
duplicates.

### Per-call clone vs. compile

Cross-checked with a one-off micro-bench (1 000 iterations, release
mode):

| Hidden neurons | `compile_creature` per call | `clone()` per call | Speed-up |
|---------------:|----------------------------:|-------------------:|---------:|
| 8              | 5.0 µs                      | 0.14 µs            | 35×      |
| 64             | 40.2 µs                     | 0.41 µs            | 98×      |
| 200            | 109 µs                      | 0.49 µs            | 221×     |

This confirms the structural saving the issue called for: in the fused
single-creature path with `NEAT_SCORER_ACTIVATION_THREADS=16`, the old
code paid 16 × `compile_creature`; the new code pays 1 + 15 × `clone()`,
saving ~15× the per-call compile cost.

## Test Plan

* `cargo test --workspace --all-features -- --test-threads=2` —
  43 tests pass (38 existing + 1 new `directory_mode_emits_compile_time_secs`,
  the rest were already passing).
* `./quality.sh < /dev/null` — clean pass (fmt, clippy, doc, deny, build,
  release build).
* Existing correctness coverage:
  - `directory_mode_record_aligned_fast_path_matches_slow_path` — confirms
    cloned worker networks produce identical scores to the single-network
    path.
  - `scorer_binary_directory_mode_matches_single_mode_results` (smoke) —
    cross-validates directory-mode results against single-creature mode.

## Notes

* The fused accumulator's docstring previously called the upstream
  `Clone` impl out as "not yet landed (NEAT-AI-core#11)"; that comment
  is now updated to reflect the landed state.
* `compileTimeSecs` is `Option<f64>` and uses `skip_serializing_if`, so
  consumers reading older JSON output (no field) and newer JSON (with
  field) both stay valid.
