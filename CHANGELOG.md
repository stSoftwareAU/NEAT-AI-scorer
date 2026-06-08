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
