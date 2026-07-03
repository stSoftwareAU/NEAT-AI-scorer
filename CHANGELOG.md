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
