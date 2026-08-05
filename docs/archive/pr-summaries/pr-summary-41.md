## Summary

Flatten the nested Rayon parallelism in `multi_score::score_from_creature_dir` so the per-chunk hot loop has exactly one `par_iter_mut` layer. A worker network pool is built up-front, sized so total parallelism equals `activation_threads`; each chunk dispatches a single flat parallel iteration over that pool. When the population meets or exceeds `activation_threads` every creature owns one worker; below that threshold, the budget is spread across creatures so a small population still saturates the CPU. Closes #41.

## Evidence

Backend / CLI change — no UI screenshot. Performance improvement verified with the Criterion bench from issue #36 (`score_from_creature_dir`), extended to cover population sizes 1, 10, 50, 200 (default 16 MiB corpus, 10 cores).

| Population | Before (median) | After (median) | Change |
|---:|---:|---:|---:|
| 1   |   30.44 ms |   25.17 ms | −14.8 % |
| 10  |  140.82 ms |  121.76 ms | −11.4 % |
| 50  |  456.39 ms |  338.12 ms | −28.0 % |
| 200 | 1558.8  ms | 1400.0  ms | −10.2 % |

Every population improved; in particular N=1 is non-regressed (the single-creature inner-split path is now the default flat distribution and is faster) and N ≥ activation_threads (10 on the bench host) shows clear gains. Reproduce with:

```sh
cargo bench -p rust_scorer --bench scoring -- score_from_creature_dir
```

## Test Plan

- `cargo test -p rust_scorer --lib` — added unit tests for the new helpers:
  - `workers_per_creature_one_per_when_population_meets_threads`
  - `workers_per_creature_distributes_remainder_when_population_below_threads`
  - `workers_per_creature_single_creature_takes_all_threads`
  - `workers_per_creature_clamps_zero_threads_to_one`
  - `partition_packed_record_ranges_covers_full_buffer_with_no_overlap`
- `cargo test -p rust_scorer --test directory_mode_tdd` — all four existing TDD tests pass unchanged (`uses_filename_stems_as_keys`, `rejects_shape_mismatch`, `rejects_forward_only_false`, `record_aligned_fast_path_matches_slow_path`).
- `cargo test -p rust_scorer --test scorer_smoke` — directory-mode smoke test (`scorer_binary_directory_mode_matches_single_mode_results`) confirms scores still match the per-creature path byte-for-byte.
- `./quality.sh` — full local gate passes (shellcheck, codespell, fmt, clippy, check, build, test, doc, release build).
