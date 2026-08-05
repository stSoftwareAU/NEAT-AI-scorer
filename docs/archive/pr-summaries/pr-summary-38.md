## Summary

Skip the `pending.extend_from_slice` memcpy when the read buffer is a whole-record multiple and `pending` is empty. The chunk is unpacked and scored directly from its source slice; only any trailing fragment (when `chunk.len() % record_bytes != 0`) is buffered for the next callback. Same change applied to both single-creature
(`rust_scorer/src/stream_score.rs::accumulate_mse_sum_forward_only_fused`) and directory-mode
(`rust_scorer/src/multi_score.rs::score_from_creature_dir`) paths. Closes #38.

## Evidence

Backend-only change — no UI to screenshot. Benchmarks run via
`BENCH_SCORING_BYTES=8388608 cargo bench -p rust_scorer --bench scoring`
on Apple M4 (10 cores), macOS 26.4.1, release profile (`lto = true`,
`codegen-units = 1`). Default env (`NEAT_SCORER_*` unset).

| Benchmark | Before (median) | After (median) | Δ median | p-value |
|---|---|---|---|---|
| `score_from_json_fused/forward_only` | 16.979 ms (471 MiB/s) | **12.347 ms (648 MiB/s)** | **−27.3 %** (−27.5 % .. −21.2 %) | < 0.05 |
| `score_from_creature_dir/creatures/10` | 63.363 ms (126 MiB/s) | **59.467 ms (135 MiB/s)** | **−6.1 %** (−6.8 % .. −3.5 %) | < 0.05 |
| `score_from_creature_dir/creatures/50` | 190.08 ms (42 MiB/s) | **158.47 ms (50 MiB/s)** | **−16.6 %** (−17.9 % .. −7.7 %) | < 0.05 |

Criterion reports each change as "Performance has improved" (significant at p < 0.05). The single-creature fused path improvement is largest because the freed memcpy was the biggest non-activation cost on that path; directory mode amortises the saved copy across N creatures so the relative win is smaller but still significant.

## Test Plan

- Added `scorer_binary_record_aligned_multi_chunk_fast_path` in
  `rust_scorer/tests/scorer_smoke.rs` — runs the binary against an
  identity-creature fixture of 1,024 records with `NEAT_SCORER_READ_BYTES=32`
  (4 records per read), forcing many fast-path callbacks; asserts
  `error≈0` and `recordCount==1024` (no records lost or double-counted).
- Added `directory_mode_record_aligned_fast_path_matches_slow_path` in
  `rust_scorer/tests/directory_mode_tdd.rs` — runs directory mode at two
  buffer sizes (`32` and `8` bytes; both record-aligned) and asserts
  per-creature `error` is bit-equivalent (within `1e-9`) and `recordCount`
  matches across both runs.
- Existing tests still pass (`cargo test -p rust_scorer`): 28 unit + 4
  directory-mode + 5 smoke + 1 partition test.
- `./quality.sh` passes cleanly (shellcheck, cargo-deny, fmt, clippy
  `-D warnings`, check, test, doc, release build).
