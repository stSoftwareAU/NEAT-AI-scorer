# Performance baseline — `rust_scorer`

Establishes the Criterion baseline for the hot scoring paths so every later
performance change can be validated with before/after evidence per the
[Performance Task Workflow](../AGENTS.md). The bench source lives at
[`rust_scorer/benches/scoring.rs`](../rust_scorer/benches/scoring.rs); reproduce
the runs with [`scripts/run-benches.sh`](../scripts/run-benches.sh) or
`cargo bench -p rust_scorer`.

## Bench groups

| Group | What it measures | Notes |
|---|---|---|
| `score_from_json_fused/forward_only` | End-to-end forward-only fused MSE accumulate over a synthetic creature + a `.bin` corpus. | Calls [`accumulate_mse_sum_forward_only_fused`](../rust_scorer/src/stream_score.rs) — the hot path the CLI runs in default mode. |
| `score_from_creature_dir/creatures/N` | Directory mode, one shared scan + `N` creatures evaluated in parallel (`N=10`, `N=50`). | Calls [`score_from_creature_dir`](../rust_scorer/src/multi_score.rs); future `multi_score.rs` work can be A/B'd against this. |
| `unpack_and_mse_inner/unpack_then_mse` | Micro-benchmark of the little-endian `f32` unpack + `mse_sum_batch_packed` inner loop on a fixed in-memory chunk (16 K records). | Mirrors the shared inner loop in `unpack_f32s_le` + `mse_sum_batch_packed` so vectorisation work can be measured in isolation. |

## Fixture parameters

| Variable | Default | Description |
|---|---|---|
| `BENCH_SCORING_BYTES` | `16777216` (16 MiB) | total bytes per `.bin` corpus |
| `BENCH_SCORING_INPUTS` | `8` | inputs per record |
| `BENCH_SCORING_OUTPUTS` | `2` | outputs per record |
| `BENCH_SCORING_HIDDEN` | `8` | hidden neurons in the synthetic creature |

The realistic perf target is the **50–200 MB** range called out in the issue.
Defaults are kept conservative so `cargo bench` finishes in a few minutes on
typical dev hardware; sweep upwards via `BENCH_SCORING_BYTES` for the full
target. **Always re-run the baseline at the same `BENCH_SCORING_BYTES`** — the
absolute numbers below are fixture-size-specific.

## Baseline — 25 April 2026

| Field | Value |
|---|---|
| Host CPU | Apple M4 (10 cores) |
| RAM | 24 GB |
| OS | macOS 26.4.1 (Darwin 25.4.0, arm64) |
| Toolchain | rustc 1.95.0 (release profile, `lto = true`, `codegen-units = 1` for `rust_scorer`) |
| Fixture | `BENCH_SCORING_BYTES=8388608` (8 MiB), `BENCH_SCORING_INPUTS=8`, `BENCH_SCORING_OUTPUTS=2`, `BENCH_SCORING_HIDDEN=8` |
| `NEAT_SCORER_*` env | unset (defaults) |
| Criterion | sample size 10 for the end-to-end groups, 100 for the micro-bench |

Numbers below are the Criterion lower / median / upper estimates (95% CI). A
"std dev" column is derived from the half-width of the CI.

| Benchmark | Lower | **Median** | Upper | Throughput (median) | Half-width ≈ stddev proxy |
|---|---|---|---|---|---|
| `score_from_json_fused/forward_only` | 15.965 ms | **16.611 ms** | 17.270 ms | 481.62 MiB/s | ±0.65 ms |
| `score_from_creature_dir/creatures/10` | 63.211 ms | **63.838 ms** | 64.480 ms | 125.32 MiB/s | ±0.63 ms |
| `score_from_creature_dir/creatures/50` | 164.95 ms | **166.63 ms** | 169.22 ms | 48.010 MiB/s | ±2.14 ms |
| `unpack_and_mse_inner/unpack_then_mse` | 1.1171 ms | **1.1663 ms** | 1.2247 ms | 535.87 MiB/s | ±0.054 ms |

Source: `BENCH_SCORING_BYTES=8388608 cargo bench -p rust_scorer` on the host above.
The 8 MiB fixture is small enough to run inside an unattended worker; future
runs at the issue's 50–200 MB target should be appended as a separate dated
section so historical numbers stay reproducible at their original size.

### Reading the directory-mode throughput

Throughput in `score_from_creature_dir` is reported relative to the shared
training corpus byte count (`BENCH_SCORING_BYTES`), **not** corpus × creature
count. Multiply by the creature count to compare against single-creature
scoring: at `N=50` the 48 MiB/s shared-scan figure corresponds to roughly
2.4 GiB/s of work performed (50 networks × 48 MiB/s).

## Refreshing the baseline

1. Run `./scripts/run-benches.sh` (default fixture) and record the median +
   std-dev proxy for each benchmark.
2. For an issue-target run, set `BENCH_SCORING_BYTES=200000000` (200 MB) and
   re-run; capture the host CPU, RAM, and OS, and append a new dated section
   above. **Do not overwrite older sections** — historical baselines are how
   regressions are detected.
3. When proposing a perf PR, paste the Criterion comparison output (or the
   before/after median + CI) into the PR summary. PRs without before/after
   evidence are rejected per `AGENTS.md`.
