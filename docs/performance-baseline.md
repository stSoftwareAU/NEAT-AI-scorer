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

## Hot spots — 25 April 2026 (Issue #37)

Sample-based flamegraphs captured with the cross-platform
[`scripts/profile-flamegraph.sh`](../scripts/profile-flamegraph.sh) pipeline
(macOS `sample` → `inferno-collapse-sample` → `inferno-flamegraph`). Equivalent
output on Linux via `cargo flamegraph` (`perf` + `inferno`). Fixture:

* Single-creature: **2 GiB** synthetic `.bin` corpus, one 8-input / 2-output
  forward-only MLP with 8 TANH hidden neurons.
* Multi-creature: **500 MB** corpus, **50** identical synthetic creatures
  loaded via directory mode.

Host: Apple M4 (10 cores), macOS 26.4.1, `profile = "profiling"`
(release + `debug = true`). The scorer ran unmodified with default env
(`NEAT_SCORER_ACTIVATION_THREADS` unset → all CPUs).

Flamegraphs committed under [`docs/evidence/`](evidence/):

* [`single-creature.svg`](evidence/single-creature.svg) — 2,255 samples
* [`multi-creature.svg`](evidence/multi-creature.svg) — 10,868 samples

### Single-creature fused path — top 5 (leaf / self time)

_Idle scheduler/wait samples excluded._ Numbers show percent of total
samples (2,255) and percent of **active CPU samples** (749). Because each
iteration of the forward-only path is small, Rayon workers spend ~67 % of
wall-clock time sleeping on `swtch_pri` / `__psynch_mutexwait`; those are
listed under the unscheduled-parallelism finding below.

| # | Function | Total % | Active % | Where it comes from | Addressed by |
|---|---|---|---|---|---|
| 1 | `tanhf` (libm activation) | 9.3 % | 27.9 % | Called from `mse_sum_batch_packed` → `mse_sum_batch_4way` inside `neat_core`. Each hidden-layer activation. | Not covered by #38–#42. Suggested **new follow-up**: vectorised / approximate TANH in `neat-core`. PGO (#43) may also help. |
| 2 | `neat_core::loss::mse_sum_batch_packed` | 8.9 % | 26.8 % | Inner fused MSE + activation loop over the unpacked f32 batch. | Indirectly improved by #40 (feed the loop from aligned `&[f32]` without the unpack copy) and #43 (PGO). |
| 3 | `_platform_memmove` | 5.9 % | 17.8 % | 72 of 133 leaf samples are under `stream_score::accumulate_mse_sum_forward_only_fused` closure — `pending.extend_from_slice(chunk)` and `pending.copy_within(head.., 0)` compaction; the remainder is inside `mse_sum_batch_packed`. | **#38** (skip copy when chunk is record-aligned and `pending` is empty) and **#39** (pre-size `pending` / tune compaction threshold) both target this directly. |
| 4 | `neat_core::loss::mse_sum_batch_4way` closure | 5.1 % | 15.2 % | Four-way unrolled inner loop body, called from `mse_sum_batch_packed`. | Improved transitively by #40 (avoids unpack stall) and #43 (PGO). |
| 5 | `DYLD-STUB$$tanhf` | 1.8 % | 5.3 % | Procedure-linkage-table trampoline for `tanhf`. Effectively part of (1). | Combined with (1); no separate sub-issue. |

### Multi-creature directory mode (50 creatures) — top 5 (leaf / self time)

_Idle scheduler/wait samples excluded._ Numbers show percent of total
samples (10,868) and percent of active samples (6,196).

| # | Function | Total % | Active % | Where it comes from | Addressed by |
|---|---|---|---|---|---|
| 1 | `tanhf` (libm activation) | 18.9 % | 33.1 % | Same path as single-creature, now stacked across 50 creature networks per chunk. | Not covered by #38–#42. Suggested **new follow-up**: vectorised / approximate TANH in `neat-core`. PGO (#43) may help. |
| 2 | `neat_core::loss::mse_sum_batch_packed` | 15.9 % | 27.8 % | Fused MSE + activation. | **#41** (flatten nested `par_iter_mut`) reduces Rayon split overhead that sits on top of this; #43 (PGO) may also help. |
| 3 | `neat_core::loss::mse_sum_batch_4way` closure | 11.5 % | 20.2 % | Inner four-way unrolled body. | Same as #2. |
| 4 | `_platform_memmove` | 4.9 % | 8.6 % | 518 of 535 leaf samples are inside `mse_sum_batch_packed` (worker-side buffer/SIMD moves in `neat-core`); only 17 / 10 868 samples come from our `score_from_creature_dir` closure (`pending.extend_from_slice`). | **#38** / **#39** address the tiny scorer-side portion; the larger share is `neat-core` territory — not in scope for these sub-issues. |
| 5 | `DYLD-STUB$$tanhf` | 4.3 % | 7.5 % | PLT trampoline for `tanhf`. Part of (1). | Same as (1). |

### Cross-scenario findings

* **Scheduler idle is high on single-creature (66.8 % of wall-clock).** The
  default activation-parallelism fan-out is too aggressive for workloads of
  this shape (8→8→2, 2 GiB corpus). Most Rayon workers spend the run sleeping
  in `swtch_pri` / `__psynch_mutexwait`. **#41** flattens multi-creature
  nesting but does not directly address single-creature over-parallelism.
  **New follow-up suggestion:** raise the `effective_workers > 1` threshold
  in `stream_score::accumulate_mse_sum_forward_only_fused` (or scale workers
  with `n_records`) so small/fast batches stay single-threaded. Captured for
  tracking alongside the other sub-issues.
* **`tanhf` dominates active CPU time in both scenarios (27.9 % / 33.1 %).**
  None of #38–#42 targets activation. Options: vectorised SIMD TANH, a
  `tanh` polynomial approximation flag on the squash path, or moving to a
  cheaper squash where the creature schema allows. This is a `neat-core`
  concern — suggested **new follow-up** to open against NEAT-AI-core.
* **Per-worker creature recompilation (#42) is not visible in the steady-state
  flamegraph.** `compile_creature` is called a fixed number of times at
  start-up and does not appear in the top-25 leaf or inclusive lists. The
  optimisation is still worthwhile for latency (cold-start) but will not
  shift the steady-state numbers recorded here.
* **Re-profile after each sub-issue lands** and overwrite `single-creature.svg`
  / `multi-creature.svg` — the Hot spots table above is expected to re-order
  once the memmove-heavy frames are gone.

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
