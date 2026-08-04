## Summary

Profiled the fused scoring hot paths end-to-end and recorded an evidence-backed
list of hot spots so the optimisation sub-issues (#38–#43) can be prioritised
by measurement rather than guesswork. Closes #37.

Captured portable, no-sudo flamegraphs via a new
[`scripts/profile-flamegraph.sh`](../scripts/profile-flamegraph.sh) helper
that wraps macOS `sample` → `inferno-collapse-sample` → `inferno-flamegraph`
(the same pipeline produces identical SVG output from `cargo flamegraph` on
Linux). Added a dedicated `profiling` Cargo profile (release + `debug = true`)
so symbols survive into the flamegraph without touching the release build.

No changes under `rust_scorer/src/` (acceptance criterion).

## Evidence

Flamegraphs committed under `docs/evidence/`:

- `docs/evidence/single-creature.svg` — 2,255 samples over a 2 GiB synthetic
  corpus, default `NEAT_SCORER_ACTIVATION_THREADS` (all CPUs).
- `docs/evidence/multi-creature.svg` — 10,868 samples over a 500 MB corpus
  with 50 synthetic creatures in directory mode.

Host: Apple M4 (10 cores), macOS 26.4.1, `profile = "profiling"`.

### Top hot spots

Full table (leaf/self vs active-CPU percentages, callers, sub-issue
mapping) lives in the new "Hot spots" section of
[`docs/performance-baseline.md`](performance-baseline.md).

**Single-creature (active CPU only):**

| # | Leaf | Active % | Addressed by |
|---|---|---|---|
| 1 | `tanhf` | 27.9 % | New follow-up (neat-core: vectorised/approx TANH). #43 (PGO) may help. |
| 2 | `mse_sum_batch_packed` | 26.8 % | #40 (zero-copy cast), #43 (PGO) |
| 3 | `_platform_memmove` | 17.8 % | #38 (skip copy when chunk is record-aligned) + #39 (pre-size + compaction threshold) |
| 4 | `mse_sum_batch_4way` closure | 15.2 % | #40, #43 |
| 5 | `DYLD-STUB$$tanhf` | 5.3 % | Same as (1) |

**Multi-creature / 50 creatures (active CPU only):**

| # | Leaf | Active % | Addressed by |
|---|---|---|---|
| 1 | `tanhf` | 33.1 % | New follow-up (neat-core) |
| 2 | `mse_sum_batch_packed` | 27.8 % | #41 (flatten nested Rayon) + #43 |
| 3 | `mse_sum_batch_4way` closure | 20.2 % | #41, #43 |
| 4 | `_platform_memmove` | 8.6 % | Mostly inside `neat-core`; only 17/10,868 samples from the scorer's own `extend_from_slice` (→ #38/#39). |
| 5 | `DYLD-STUB$$tanhf` | 7.5 % | Same as (1) |

### Cross-scenario findings

- Single-creature spends **66.8 %** of wall-clock in Rayon sleep/yield
  (`swtch_pri` + `__psynch_mutexwait`). The default activation fan-out is
  too aggressive for small/fast batches; none of #38–#42 directly targets
  this. Flagged as a suggested new follow-up in `performance-baseline.md`.
- `tanhf` is the single biggest active-CPU cost in both scenarios (28 %
  and 33 %) and is not covered by the current plan. Flagged as a
  suggested new neat-core follow-up.
- `compile_creature` (the target of #42) does not appear in steady-state
  stacks — #42 is latency-only.

## Test Plan

- `./quality.sh` passes cleanly (shellcheck, cargo-deny, fmt, clippy,
  tests, rustdoc, release build, spell-check).
- `scripts/profile-flamegraph.sh` end-to-end reproduces both SVGs on the
  host above:
  - default size (`2 GiB` single / `500 MB × 50` multi) → the checked-in
    flamegraphs.
  - scaled-down smoke (`209715200 52428800 10`) completes in ≈ 2 s; used
    during development to validate the pipeline.
- `.codespellrc` extended to skip `docs/evidence/*.svg` (the SVGs contain
  truncated thread/symbol names from the OS sampler); `scripts/spell-check.sh`
  still passes and still flags genuine typos elsewhere.
- No functional Rust changes — `cargo test --workspace --all-features`
  passes with 3 + 4 + unit suites green.

Per the acceptance criteria there are no code changes under
`rust_scorer/src/`; the `profiling` profile lives in the workspace
`Cargo.toml` and the helper script is additive.
