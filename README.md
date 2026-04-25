# NEAT-AI-scorer

Native **MSE scorer** CLI for NEAT-AI creatures. Shared logic lives in **`neat-core`**, resolved from a **path dependency** on **[NEAT-AI-core](https://github.com/stSoftwareAU/NEAT-AI-core)** (see `rust_scorer/Cargo.toml`). GitHub Actions checks out `NEAT-AI-core` next to this repo so CI can resolve that path.

## Source

| Component | Provenance |
|-----------|----------------|
| `rust_scorer/` | **`training_bin_stream::for_each_read_chunk`** (pipelined on native, same API on wasm) plus **pending + head + compact** (`stream_score.rs`), fused MSE when `forwardOnly` is true; **`float_scan_bench`** uses the same reader for throughput experiments. |
| `neat-core` (crate) | **`../../NEAT-AI-core/neat-core`** relative to `rust_scorer/Cargo.toml` — clone **NEAT-AI-core** as a sibling of **NEAT-AI-scorer** (same parent directory). |
| `LICENSE`, `.gitleaks.toml` | `origin/Develop` of NEAT-AI |

## Build

```bash
./quality.sh
```

Or step-by-step (matches CI):

```bash
export RUSTFLAGS="-D warnings"
cargo deny check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::filter_next -D clippy::collapsible_if
cargo test --workspace --all-features
cargo build --release -p rust_scorer
```

Requires **shellcheck**, **cargo-deny** (`cargo install cargo-deny --locked`), **codespell** (`pip install --user codespell`, used by `scripts/spell-check.sh`), and optionally **cargo-edit** for the upgrade step in `./quality.sh`.

### Spell check

CI runs `codespell` via `scripts/spell-check.sh`; the same script is invoked by `./quality.sh`, so the local gate and CI stay in lock-step. Reproduce the CI spell check at any time with:

```bash
./scripts/spell-check.sh
```

Configuration (ignore list, skip paths, check-filenames / check-hidden flags) is kept in a single source of truth: [`.codespellrc`](./.codespellrc). When a domain term trips codespell, prefer adding it — with a short justification comment — to `.codespellrc` over silencing the whole file. Genuine typos must continue to fail the build. Current curated domain entries:

- `renderD` — DRM device node name (e.g. `renderD128`).
- `mape` / `MAPE` — Mean Absolute Percentage Error (a `neat-core` loss function).

Binaries: `rust_scorer`, `float_scan_bench` (see `rust_scorer/Cargo.toml`).

## CLI

Positional arguments only (same contract as in NEAT-AI):

```text
rust_scorer <creature.json | creatures_dir> <training_data_dir>
```

- `creature.json` path: scores one creature and returns the existing single-object output.
- `creatures_dir` path: scores every `*.json` in that directory in one pass over training data and returns one JSON object keyed by each file's stem (filename without extension or folders).
- Directory mode requires `forwardOnly: true` and matching `input` / `output` shape across all files.

### Stdin input mode

For restricted worker/sandbox environments where writing a temp file may fail
even with write permission (see issue #15), pass the creature JSON on stdin:

```text
rust_scorer --creature-stdin <training_data_dir>
```

The positional contract is unchanged in default mode. With `--creature-stdin`,
the binary reads creature JSON from standard input until EOF and expects a
single positional argument (`<training_data_dir>`).

### Output

Single-creature mode JSON includes **`forwardOnly`** (from the creature) and **`trainingReadBackend`**: on a native release build you should see **`pipelined_double_buffer`** when `forwardOnly` is `true` (fused scoring + `training_bin_stream`). If `forwardOnly` is `false`, you get **`record_iterator`** instead (no pipelining — much slower on large data).

In directory mode, output is a top-level object keyed by creature filename stem, where each value has the same shape as a single-creature `ScoreResult`.

```json
{
  "GRQ-10-1": {
    "score": 0.9999998,
    "error": 0.0
  },
  "GRQ-12-1": {
    "score": 0.9999998,
    "error": 0.0
  }
}
```

This mode uses one shared training-data scan and parallelises scoring across creatures to use available CPU cores by default.

Per-chunk activation runs through a single flat Rayon layer (Issue #41): a worker network pool sized to `activation_threads` is built up-front, and every chunk dispatches one `par_iter_mut` over that pool. When the population meets or exceeds `activation_threads` each creature owns one worker; below that, the thread budget is spread across creatures so a small population still saturates the CPU. The JSON output keeps the same shape — `parallelActivationBatches` and `maxActivationBatchRecords` are not emitted in directory mode.

For forward-only single-creature fused scoring, activation parallelism also
defaults to all available CPU cores. Set `NEAT_SCORER_ACTIVATION_THREADS` only
when you want to tune down/up manually.

## Local layout

Place **NEAT-AI-core** and **NEAT-AI-scorer** as **siblings** (e.g. `…/src/NEAT-AI-core` and `…/src/NEAT-AI-scorer`). The path in `rust_scorer/Cargo.toml` is `../../NEAT-AI-core/neat-core` so `cargo build` resolves `neat-core` from your local **NEAT-AI-core** tree. CI does the same via a second checkout (`../NEAT-AI-core`).

## Relationship to NEAT-AI

Scorer-specific Rust stays here; **`neat-core`** tracks **NEAT-AI-core**.

## Related Repositories

The NEAT-AI project is split across several repositories. This repo, **NEAT-AI-scorer**, is the native MSE scorer CLI consumed by **NEAT-AI** during training.

| Repository | Role |
|------------|------|
| [NEAT-AI](https://github.com/stSoftwareAU/NEAT-AI) | Deno/TypeScript orchestrator — the NEAT neural-network runtime that trains and evaluates creatures. |
| [NEAT-AI-core](https://github.com/stSoftwareAU/NEAT-AI-core) | Shared Rust computation library (`neat-core` crate) with fused batch losses and `training_bin_stream`. |
| [NEAT-AI-Discovery](https://github.com/stSoftwareAU/NEAT-AI-Discovery) | Rust discovery module invoked by NEAT-AI via Deno FFI. |
| [NEAT-AI-Snapshot](https://github.com/stSoftwareAU/NEAT-AI-Snapshot) | Snapshot storage for trained creatures. |
| [NEAT-AI-scorer](https://github.com/stSoftwareAU/NEAT-AI-scorer) | **This repo** — native MSE scorer CLI; depends on `neat-core` via path dependency. |
| [NEAT-AI-Explore](https://github.com/stSoftwareAU/NEAT-AI-Explore) | Visualiser that consumes NEAT-AI-Snapshot data. |
| [NEAT-AI-Examples](https://github.com/stSoftwareAU/NEAT-AI-Examples) | Usage examples built on NEAT-AI. |

### Dependency graph

```mermaid
graph TD
    Examples[NEAT-AI-Examples] --> NEAT[NEAT-AI]
    NEAT -->|Deno FFI| Discovery[NEAT-AI-Discovery]
    NEAT -->|spawns CLI| Scorer[NEAT-AI-scorer]
    NEAT -->|writes| Snapshot[NEAT-AI-Snapshot]
    Scorer -->|path dep| Core[NEAT-AI-core]
    Discovery -->|path dep| Core
    Explore[NEAT-AI-Explore] -->|reads| Snapshot
```

## Why MSE-only?

The CLI scores creatures with **mean squared error** only — there is no `--cost` flag
and no runtime dispatch across loss functions.

- **Fused fast path is MSE.** The forward-only path calls
  `neat_core::loss::mse_sum_batch_packed` directly so error accumulation stays
  inside the same SIMD-friendly pass that reads packed `[inputs..., targets...]`
  records. The non-fused recurrent path (`forwardOnly: false`) uses
  `neat_core::mse_mean_record` to match the TypeScript `MSE.calculate()` mean.
- **Scope matches today's callers.** NEAT-AI `Develop` invokes this binary with
  the fixed positional contract `<creature.json> <data_dir>` (see `AGENTS.md`)
  and never requests a non-MSE score. `GROWTH_COST` and the fitness formula in
  `scoring.rs` are defined against MSE.
- **`neat-core` still exposes the full set.** The sibling crate already ships
  fused batch variants for MAE, cross-entropy, MAPE, MSLE, and hinge
  (`neat_core::loss::{mae,cross_entropy,mape,msle,hinge}_sum_batch_packed`).
  Re-adding a `--cost` dispatch would be CLI wiring plus tests — no new math —
  but until a downstream caller needs it, keeping the surface area small wins
  on KISS grounds and preserves the stable positional CLI contract.

If a downstream caller ever needs non-MSE scoring at this boundary, the
existing fused batch-packed losses in `neat-core` are the drop-in entry points;
see the in-tree `rust_scorer/` experiment on
[`milestone/pure-rust-scorer-experiment`](https://github.com/stSoftwareAU/NEAT-AI/blob/milestone/pure-rust-scorer-experiment/rust_scorer/src/cost.rs)
for the six-way dispatch pattern.

## CI

### Job dependency graph (Issue #23)

`.github/workflows/ci.yml` declares an explicit job graph so ordering is
predictable on re-runs and partial failures:

```mermaid
graph LR
    validation[validation] --> quality[quality]
    validation --> security[security]
    quality --> ciRequired[ci-required<br/>aggregator]
    security --> ciRequired
    shellChecks[shell-checks] --> ciRequired
    spellCheck[spell-check] --> ciRequired
```

* **`validation`** is the foundation — it verifies required files and Cargo
  metadata. `quality` and `security` `needs: [validation]` so a broken repo
  layout fails fast without burning Rust compile minutes or a security scan.
* **`shell-checks`** and **`spell-check`** are lightweight and run in
  parallel with the foundation to surface findings early.
* **`ci-required`** is a single fan-in aggregator. It `needs:` every gating
  job, uses `if: always()` so it always reports a result, and inspects
  `needs.<job>.result` in its run step to fail unless every dependency
  reported `success` or `skipped`. **Branch protection should pin exactly
  one required check — `CI Required Checks` — so the merge gate is stable
  even when individual gating jobs are added or renamed.**

The graph is validated by `scripts/check-ci-job-graph.sh` (wired into
`quality.sh`) and covered end-to-end by `tests/scripts/workflow_job_graph.bats`.

### Other PR automation

Besides the quality gate (`.github/workflows/ci.yml`), PRs also run a guarded
auto-version increment job (`.github/workflows/version-increment.yml`,
Issue #20). On each PR the job compares `rust_scorer/Cargo.toml` against the
base branch and, if the version has not already changed on the branch, bumps
the patch component once and pushes that commit back to the PR branch. A
re-run of CI — or a human-authored bump on the same branch — short-circuits
the job, so no duplicate bump commits are produced. The underlying logic
lives in `scripts/version-increment.sh` and is covered by
`tests/scripts/version_increment.bats`.

PRs also run an auto-format job (`.github/workflows/auto-format.yml`,
Issue #19). The job runs `cargo fmt --all` on the PR branch; if the working
tree changes, the formatting fix is committed with a deterministic message
and pushed back. When there are no changes the commit step is skipped, so
re-running on a clean branch is a no-op. Change detection and the commit
message live in `scripts/auto-format.sh` and are covered by
`tests/scripts/auto_format.bats`; the workflow itself is validated by
`scripts/check-auto-format-workflow.sh` (invoked from `quality.sh`).

### GitHub Actions version policy (Node 24 compat)

GitHub is deprecating the Node 20 runtime for JavaScript actions, so the
workflow files pin each `uses:` reference to a major that runs on Node 24
where one exists. The policy — minimum majors, tracked Node 20 exceptions
(`actions/dependency-review-action@v4`, `rustsec/audit-check@v2` — no Node
24 release upstream yet), and composite/shell allow-list — is encoded in
`scripts/check-workflow-action-versions.sh` and validated end-to-end by
`tests/scripts/workflow_action_versions.bats`. `quality.sh` invokes the
script so any workflow that adds an unpinned or outdated `uses:` reference
fails the local gate before CI (Issue #24).

## How to bench

`rust_scorer` ships a Criterion suite (Issue #36) covering the forward-only
fused path, the multi-creature directory mode, and the inner unpack + MSE
loop. The bench is **not** part of `quality.sh` — Criterion runs are slow and
not deterministic enough for a merge gate. Reproduce the baseline with:

```bash
./scripts/run-benches.sh
```

Or, for the issue's 50–200 MB target corpus:

```bash
BENCH_SCORING_BYTES=200000000 ./scripts/run-benches.sh
```

Tunables (all optional): `BENCH_SCORING_BYTES`, `BENCH_SCORING_INPUTS`,
`BENCH_SCORING_OUTPUTS`, `BENCH_SCORING_HIDDEN`. Recorded baselines and
host-specific numbers live in [`docs/performance-baseline.md`](docs/performance-baseline.md).
Per `AGENTS.md`, performance PRs without before/after Criterion evidence are
rejected.

## How to flamegraph (Issue #37)

Capture an SVG flamegraph for the single-creature fused path and the
multi-creature directory mode with the cross-platform helper:

```bash
./scripts/profile-flamegraph.sh
```

The defaults (2 GiB single-creature corpus, 500 MB × 50 creatures
multi-creature corpus) take a couple of minutes end-to-end on Apple
silicon and write:

* `docs/evidence/single-creature.svg`
* `docs/evidence/multi-creature.svg`

The script builds `rust_scorer` with a dedicated `profiling` Cargo profile
(release optimisations + `debug = true`) so function names survive into the
flamegraph, generates deterministic synthetic data via Python's `array`
module, runs each scenario and feeds raw samples through `inferno`.

One-time install of the toolchain:

```bash
cargo install inferno           # required for inferno-collapse-sample + inferno-flamegraph
cargo install flamegraph        # Linux only: provides `cargo flamegraph` on top of `perf`
# macOS: Xcode command-line tools ship /usr/bin/sample — no sudo needed.
```

Runtime tunables (override any default positional arg or env var):

```bash
# size-down for a quick smoke test
./scripts/profile-flamegraph.sh 209715200 52428800 10

# slim inputs / fewer hidden neurons
PROFILE_NUM_INPUTS=4 PROFILE_HIDDEN=4 ./scripts/profile-flamegraph.sh
```

The `profiling` Cargo profile inherits from `release` but turns debug info
and symbols back on. It is invoked only by this helper — the CI `release`
build path is unchanged.

Interpretation and the current top-5 cost centres per scenario are
documented in the **"Hot spots"** section of
[`docs/performance-baseline.md`](docs/performance-baseline.md). Re-run the
script after each optimisation sub-issue lands and overwrite the SVGs so
the hot-spot ordering stays honest.

## License

Apache-2.0 — see `LICENSE`.
