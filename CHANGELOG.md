# Changelog

All notable changes to **NEAT-AI-scorer** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The `rust_scorer` package version in
[`rust_scorer/Cargo.toml`](./rust_scorer/Cargo.toml) is bumped automatically
on each pull request by the
[Version Increment](./.github/workflows/version-increment.yml) workflow, so
this changelog is the human-readable record of *what* changed between
versions. Add new entries under `## [Unreleased]`; on release, rename that
section to the released version with its date.

## [Unreleased]

### Added

- **`RMSE` cost function — `--cost RMSE` (Issue #337).** Adds Root Mean Squared
  Error to the `--cost` selector. `RMSE` reuses the existing MSE squared-error
  accumulation unchanged on **both** the CPU and GPU paths (the
  `forward_mse_batched` kernel is shared — **no new kernel**) and differs only by
  a single host-side `sqrt` applied at finalisation via the shared
  `CostKind::finalise_mean` helper. It therefore ranks creatures identically to
  `MSE` while reporting interpretable, same-unit magnitudes, and carries **no
  performance difference versus `MSE`** on either backend. The resolved name is
  echoed back as the `costName` JSON field, and `--gpu on` accepts `RMSE` (it is
  GPU-supported, not a hard error). Documented in the README "Cost function
  selector" section.

- **Record-level `--sample-rate` sub-sampling in the forward-only streaming
  reader (Issue #310, multi-fidelity fitness).** New `--sample-rate <f>`
  (`(0, 1]`, default `1`) and `--sample-phase <u64>` (default `0`) flags make the
  reader deterministically keep a stratified subsample of the corpus — record
  `i` is kept iff `floor((i+1)·rate) > floor(i·rate)` — in a single pass with **no
  second corpus on disk**. The stride matches the TypeScript consumer
  (NEAT-AI#3257) so both agree on which records survive, and a stateful sampler
  threads the global record index through `run_io_loop` so the kept set is
  independent of chunk boundaries. Sampling applies uniformly to the fused
  single-creature, multi-creature CPU, GPU directory, and recurrent paths. When
  sub-sampling runs, `error`/`score` are over the kept subset, `recordCount` is
  the sampled count, and a new `sampleRate` JSON field echoes the effective rate
  (absent for a full-corpus run, so the default JSON is unchanged). Out-of-range
  rates fail loud with a non-zero exit. Synthetic CPU benchmark (1.5 M records,
  4→128→2 forward-only creature): `0.5`→1.76×, `0.25`→3.15×, `0.1`→5.67×
  wall-clock speed-up. Lighting this up on the production corpus is gated on
  production data + a human (rank-correlation gate on NEAT-AI#3256 / #3257); the
  scorer does not auto-release.
- **Expand GPU squash coverage to every point-wise activation (Issue #305).**
  The `forward_mse_batched` / `forward_mse_scratch` WGSL kernels now inline all
  32 point-wise squashes (`SquashType` 0..=31 — SELU, GELU, SINE, ABSOLUTE,
  BENT_IDENTITY, Cube, HARD_TANH, …), matching the CPU `apply_squash` +
  `apply_limit_range` pipeline, instead of only IDENTITY/RELU/LOGISTIC/TANH. A
  production creature mixing the wider set is now GPU-hostable rather than
  falling back to CPU on ~95.8 % of its neurons (Scorer#299). The six aggregate
  squashes (32..=37: MINIMUM/MAXIMUM/IF/HYPOT/HYPOTv2/MEAN) combine the
  individual weighted inputs rather than their sum, so they stay CPU-only.
  Constant neurons are also rejected by the pre-flight
  (`GpuPrepareError::ConstantNeuron`) — the CPU returns a clamped bias and
  ignores their synapses, which the kernel cannot reproduce, so a creature
  carrying one (the GRQ creature has 3) falls back to CPU rather than being
  silently mis-scored. CPU↔GPU parity across all 32 point-wise squashes is asserted on Apple M4 /
  Metal by `cpu_vs_gpu_pointwise_squash_coverage`. The CPU path and the
  `auto_should_use_gpu` per-path default are unchanged; a new optional
  `BENCH_SCORING_HIDDEN_SQUASH` bench env var drives the GPU-vs-CPU A/B on the
  production squash mix.

### Changed

- **Drop the redundant `cargo check` step from the CI `quality` job (Issue
  #403).** `cargo clippy` (Run linter) drives the same rustc front-end over the
  identical `--all-targets --all-features` scope with `-D warnings`, so it is
  the strict type-check gate — the separate `Check types` step could only pass
  once clippy already had and did not reuse clippy's artefacts, adding wall-clock
  to the heaviest job for no coverage. The README "matches CI" block, its
  alignment guard (`scripts/check-readme-ci-alignment.sh`), and `AGENTS.md` are
  updated to match; the guard now rejects a re-introduced standalone
  `cargo check`.
- **Document the recommended `NEAT_SCORER_READ_BYTES` for large-record GRQ
  hosts (Issue #307).** Swept `NEAT_SCORER_READ_BYTES` ∈ {2, 8, 16, 32, 64} MiB
  on the #296 production fixture (9848-byte records). Larger aligned reads
  recover ~20–24 % on the single-creature and multi-creature production groups
  (sweet spot 16–32 MiB) by giving the Rayon worker pool bigger per-chunk
  batches — a chunk-count amortisation effect that only helps large records.
  The global default stays **2 MiB** (the gain is record-size specific and the
  sweep host was contended, not the quiet host the merge gate requires); the
  README now documents exporting `NEAT_SCORER_READ_BYTES=33554432` (32 MiB) on
  GRQ hosts, and `docs/performance-baseline.md` records the sweep table. No code
  or default change.
- **Acknowledge the neat-core 0.1 -> 0.2 breaking bump in the version-baseline
  gate (Issue #252).** `neat-core.expected-version` recorded `0.1.46` while the
  sibling neat-core had advanced to the 0.2 line, so `check-neat-core-version.sh`
  (CI `validation` job) and the `neat_core_version_gate.bats` real-repository
  test both failed the breaking-bump gate. The only breaking boundary crossed is
  neat-core #177 (`SynapseData::from_index` u32 -> u16), whose scorer handling
  already landed in milestone #257; a clean `cargo build -p rust_scorer` and the
  full scorer test suite pass against the sibling. Bumped the recorded baseline
  to `0.2.5` to record that handling; patch drift within the 0.2 line is
  non-breaking.

### Added

- **Crate-level rustc lint hardening (Issue #274).** The workspace previously
  configured only Clippy lints, leaving rustc's own (`rust`) lint groups
  unenforced at the source-tree level. The root `Cargo.toml` now declares a
  `[workspace.lints.rust]` table denying `unsafe_op_in_unsafe_fn` and `unused`
  (inherited by `rust_scorer` via `[lints] workspace = true`), and
  `rust_scorer/src/lib.rs` adds `#![warn(missing_docs)]` scoped to the library
  surface (the binary targets are doc-noisy). Per-lint denies are used instead
  of a blanket `#![deny(warnings)]` so a future compiler warning does not break
  the build unexpectedly. The posture is validated by
  `scripts/check-rust-lints.sh` (run locally via `./quality.sh`) and covered
  end-to-end by `tests/scripts/rust_lints.bats`. Documented in the README.

- **CI gate against an unhandled breaking neat-core bump (Issue #252).** The
  `neat-core` dependency stays an unpinned `path` dependency that tracks head
  (kept by design), so a new version-baseline gate now guards against silently
  consuming a breaking change. Scorer records the last-handled neat-core
  version in the checked-in `neat-core.expected-version` file;
  `scripts/check-neat-core-version.sh` (run in the CI `validation` job and
  locally via `./quality.sh`) reads the sibling
  `../NEAT-AI-core/Cargo.toml` `[workspace.package] version` and **fails**
  when neat-core's breaking component (major for `>= 1.0`, minor for pre-1.0)
  exceeds the baseline — forcing a deliberate upgrade. Documented in the
  README and CONTRIBUTING; covered by `tests/scripts/neat_core_version_gate.bats`.

- **stderr note when a non-MSE `--cost` forces CPU fallback in directory
  mode (Issue #205).** Under the default `--gpu auto`, selecting a non-MSE
  `--cost` makes `auto_should_use_gpu` return false, so the directory path
  runs on CPU. The fallback was otherwise silent — only the
  `gpuBackend: "cpu-fallback"` JSON field hinted at it. The scorer now prints
  one informational `[gpu] auto fallback to CPU directory mode: cost <NAME>
  is not GPU-supported ...` line to stderr, mirroring the existing
  `--gpu on` / GPU-runner fallback messages and naming the cost as the reason.
  MSE / GPU-supported costs and explicit `--gpu on|off` are unaffected
  (no extra output). New `gpu::auto_cost_fallback_note` helper, unit-tested
  in `gpu/mod.rs` and end-to-end in `tests/directory_mode_tdd.rs`.
- **CI lint gate for GitHub Actions (Issue #195).** New
  `.github/workflows/actionlint.yml` runs [actionlint](https://github.com/rhysd/actionlint)
  on every pull request and on pushes to the default branches, so workflow
  regressions (invalid `runs-on` labels, broken `${{ }}` expressions, unknown
  `uses:` inputs, shellcheck findings in `run:` scripts) fail the build. The
  actionlint binary is downloaded from a version-pinned upstream release — no
  third-party wrapper action enters the supply chain. The workflow is validated
  by `scripts/check-actionlint-workflow.sh` (invoked from `quality.sh`) and
  covered end-to-end by `tests/scripts/actionlint_workflow.bats`.
- **Large creatures now run on the GPU (Issue #182).** New
  `forward_mse_scratch` WGSL kernel holds each thread's activations in a
  runtime-sized `storage` buffer instead of the 256-element `private` array of
  `forward_mse_batched`, lifting the 256-neuron cap. The host routes creatures
  above 256 neurons to the new kernel and bounds the activation scratch with a
  memory budget (`NEAT_SCORER_GPU_SCRATCH_BYTES`, capped to the device's max
  storage-buffer binding size) plus a grid-stride loop over records. CPU↔GPU
  parity is verified up to 4010-neuron creatures.
- `CONTRIBUTING.md` — contributor guide summarising the local gate
  (`./quality.sh`), prerequisites, coding standards, and the pull request
  workflow
- `CHANGELOG.md` — this file, following Keep a Changelog, to record changes
  alongside the automated `rust_scorer` version bumps.

### Changed

- **`./quality.sh`'s `cargo upgrade` step is now opt-in (Issue #210).** The
  default gate is read-only against `Cargo.lock` / `Cargo.toml` — running
  `./quality.sh` to validate an unrelated change no longer bumps dependency
  versions in the working tree. The upgrade behaviour is preserved behind an
  explicit opt-in: `./quality.sh --upgrade` or `QUALITY_UPGRADE=1 ./quality.sh`.
  The step now lives in `scripts/cargo-upgrade.sh`, covered end-to-end by
  `tests/scripts/cargo_upgrade.bats`. Routine, quarantine-gated bumps continue
  to go through `./bump-deps.sh` (Issue #105).
- **Input-reachable `assert!`/serialise `expect` panics are now structured
  errors (Issue #201).** `scoring::value_penalty`, `compute_score_components`,
  `complexity_penalty` and `calculate_score` return `Result<_, String>` and
  emit a clear message for negative/non-finite weights, biases or average
  errors instead of aborting the process via a release-build panic. The
  `main` serialisation step routes `serde_json` failures through the standard
  `eprintln!("Error: ...")` + `exit(1)` path rather than `expect`. Pure
  internal-math invariants (penalty/score bounds) stay as `debug_assert!`.
- The GPU pre-flight (`multi_score::gpu_directory_compatible`) and
  `build_batched_network_data` no longer reject creatures above 256 neurons —
  they route to `forward_mse_scratch`. Only an unsupported squash, a shape
  mismatch, or an absurd neuron count (> `MAX_NEURONS_ABSOLUTE`) now forces a
  CPU fallback (Issue #182). The shared WGSL `squash` clamps its input so large
  pre-activations cannot overflow Metal's `tanh`/`exp` to `NaN`.
- CI now enforces the documentation floor: `CONTRIBUTING.md` and
  `CHANGELOG.md` are listed in the `validation` job's required-files check
  (`.github/workflows/ci.yml`), guarded by `tests/scripts/docs_floor.bats`.
- `SECURITY.md` now documents an emergency dependency-bump procedure that
  points a responder at `bump-deps.sh --quarantine-hours 0` for an out-of-band
  supply-chain fix. `scripts/check-security-policy.sh` enforces the new section
  as a sixth rule (Issue #171).
