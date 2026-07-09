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

- **Early-exit / partial-score API for directory-mode batch scoring
  (Issue #308).** New library entrypoint
  `multi_score::score_from_creature_dir_with_early_exit` mirrors
  `score_from_creature_dir` but invokes a caller-supplied callback after each
  scored chunk with a `PartialScore` snapshot (running mean error + records
  scored) per still-active creature. The callback returns an `EarlyExit`
  directive — `Continue`, `AbortCreatures(indices)` (freeze those creatures at
  their partial score for the rest of the corpus), or `AbortAll` (stop the
  sweep). Aborted creatures skip all remaining activation work, which is the
  wall-clock saving. This unblocks NEAT-AI#3264's cascading / early-abort
  fitness ranking without reimplementing the fused scoring loop in TypeScript.
  **Full-score parity:** with no callback (or a callback that always returns
  `Continue`) scores are bit-identical to the old path — verified by
  `tests/early_exit_tdd.rs`. **Benchmark (Issue #308 gate,
  `BENCH_SCORING_BYTES=32 MiB`, synthetic 8→8→2 population, aborting 50 % of
  creatures after the first chunk):** directory-mode median wall-clock drops
  **40.4 %** at N=50 (2.02 s → 1.20 s) and **45.1 %** at N=200 (4.77 s →
  2.62 s) — far above the ≥5 % merge gate. Single-creature path unchanged.

### Changed

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
