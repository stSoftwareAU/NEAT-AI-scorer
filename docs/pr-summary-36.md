## Summary
Stand up Criterion benchmark infrastructure for `rust_scorer` and capture an initial baseline so every later perf change can be validated with before/after evidence per `AGENTS.md`. Closes #36.

* Added `criterion = "0.8"` (with `html_reports`) to `rust_scorer/Cargo.toml` `[dev-dependencies]` and a `[[bench]]` entry pointing at `rust_scorer/benches/scoring.rs` with `harness = false`. (The issue requested `0.5`; `quality.sh` runs `cargo upgrade --incompatible` so the manifest pins the auto-upgraded `0.8` — same crate, latest API.)
* Added `rust_scorer/src/lib.rs` exposing `scoring`, `multi_score`, `stream_score`, and `read_tuning` so the bench (a separate compilation target) can call the same hot paths the CLI runs. The binary in `src/main.rs` is untouched — its module tree, positional CLI contract, and tests are unchanged.
* New bench groups in `benches/scoring.rs`:
  * `score_from_json_fused/forward_only` — end-to-end fused MSE accumulate.
  * `score_from_creature_dir/creatures/{10,50}` — directory mode at two N values.
  * `unpack_and_mse_inner/unpack_then_mse` — micro-bench of the `unpack_f32s_le + mse_sum_batch_packed` inner loop on a fixed in-memory chunk.
* Fixture sizes are parameterised through `BENCH_SCORING_BYTES`, `BENCH_SCORING_INPUTS`, `BENCH_SCORING_OUTPUTS`, `BENCH_SCORING_HIDDEN`. Defaults are conservative (16 MiB) to keep `cargo bench` runtime sane; the issue's 50–200 MB target is reachable via `BENCH_SCORING_BYTES=200000000`.
* Added `scripts/run-benches.sh` (one-command reproducer; **not** wired into `quality.sh` per the issue) plus `tests/scripts/run_benches.bats` covering argument forwarding, env-var pass-through, exit-status propagation, and the configuration banner.
* Added `docs/performance-baseline.md` documenting the host (Apple M4 / 24 GB / macOS 26.4.1), the bench fixtures, and the recorded medians + CI half-widths for each group; updated `README.md` with a "How to bench" section linking to it.

## Evidence

This is performance infrastructure (no perf change), so the baseline numbers themselves are the evidence. Captured on Apple M4 / 10 cores / 24 GB / macOS 26.4.1, `BENCH_SCORING_BYTES=8388608` (8 MiB):

| Benchmark | Lower | **Median** | Upper | Throughput |
|---|---|---|---|---|
| `score_from_json_fused/forward_only` | 15.965 ms | **16.611 ms** | 17.270 ms | 481.62 MiB/s |
| `score_from_creature_dir/creatures/10` | 63.211 ms | **63.838 ms** | 64.480 ms | 125.32 MiB/s |
| `score_from_creature_dir/creatures/50` | 164.95 ms | **166.63 ms** | 169.22 ms | 48.010 MiB/s |
| `unpack_and_mse_inner/unpack_then_mse` | 1.1171 ms | **1.1663 ms** | 1.2247 ms | 535.87 MiB/s |

Full numbers and the host description live in `docs/performance-baseline.md`.

## Test Plan

- Added `tests/scripts/run_benches.bats` (5 cases) — verifies `scripts/run-benches.sh` invokes `cargo bench -p rust_scorer`, forwards extra args, propagates env vars + exit status, and prints the fixture banner. Runs in `quality.sh` via the existing bats step.
- Existing test suites (`cargo test --workspace`, the bin/integration smoke tests, the bats scripts) continue to pass — the lib + bin split does not change main.rs's module tree.
- `cargo check --workspace --all-targets` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` cover the bench source (it compiles into the test build graph), so any future bench change is gated by `quality.sh` even though `cargo bench` is not.
- `cargo bench -p rust_scorer` was executed end-to-end with `BENCH_SCORING_BYTES=8388608` to populate the baseline doc above.
- `./quality.sh < /dev/null` passes.
