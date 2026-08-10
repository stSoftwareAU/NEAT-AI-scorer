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

### Fixed

- **PR auto-format syncs `Cargo.lock` to latest neat-core (Issue #542).** The
  auto-format job now runs `cargo update -p neat-core` after `cargo fmt`, so
  every PR refreshes the path-dependency lock entry against the checked-out
  NEAT-AI-core `Develop`. That stops workers reprinting
  `Updating neat-core vX -> vY` after every `model_fetch` hard-reset of a
  stale lock. The Issue #252 `neat-core.expected-version` gate is unchanged
  (still deliberate). This PR also acknowledges neat-core `0.9.0` in the
  baseline and commits the matching lock sync.

### Added

- **Parallel training-data file reads (Issue #529).** The forward-only fused
  path now streams a multi-file corpus through several concurrent `.bin`
  readers instead of one, removing the serial `f32` unpack and the per-chunk
  fork/join barrier from the critical path. One reader per CPU by default
  (capped at the file count); `NEAT_SCORER_FILE_THREADS=1` restores the single
  sequential reader. Each reader gets its share of the activation budget and of
  one shared read-buffer budget, so neither threads nor memory grow with the
  file count. Measured on an Apple M4 over a 200 MB corpus in 26 files:
  **−56.8 %** wall-clock at 40 B/record and **−45.3 %** at the production
  9848 B record width (`fused_multi_file` Criterion group,
  `docs/performance-baseline.md`). Scores are unchanged — the kept record set is
  identical at every reader count, including under `--sample-rate`. New
  `fileReadWorkers` JSON field reports the resolved reader count when `> 1`.

### Changed

- **RMSE docs separate ordering from the reported score (Issue #556).** The
  README cost table and its prose said `RMSE` "ranks identically to MSE", which
  reads as though `RMSE` is redundant. Both now state the narrower truth —
  `sqrt` is monotonic, so the creature *ordering* matches `MSE`, while the
  *reported score* differs, being in the target's own units — and the
  `CostKind::Rmse` rustdoc says the same. New `scripts/check-rmse-docs.sh`
  (in `quality.sh`, covered by `tests/scripts/rmse_docs.bats`) keeps the
  distinction from collapsing again. Documentation only: `--cost RMSE` and its
  computation are unchanged.

- **One emitter for the creature JSON wire format (Issue #513).** The envelope
  (`input`/`output`/`forwardOnly`/`semanticVersion`) plus the per-neuron and
  per-synapse literal shapes were hand-encoded with `format!` across benches,
  binaries, integration tests and both `src/` fixture modules, so a schema
  change upstream meant the same edit fifteen times. New
  `rust_scorer::fixture_json` owns the emission — `neuron_json`,
  `synapse_json`, `typed_synapse_json`, `creature_envelope`, and the
  `dense_mlp_creature_json` builder that collapses the byte-identical
  `synthetic_creature_json` triplet in the GPU parity tests. Callers keep their
  own loops, shapes and weight formulas, which differ between fixtures on
  purpose.

- **The docs private-repo guard covers the whole archive (Issue #510).** Four
  archived PR summaries — the ones added by the private-repo-reference audit
  itself — still named the private production-data and cluster-data
  repositories, and `scripts/check-docs-private-repo-refs.sh` carved three of
  them out of `in_scope()` on the rationale that they must quote what they
  removed. They need not: all four are now worded at concept level, the
  carve-out is gone, and the match is case-insensitive with `_`/`-` treated as
  word boundaries so lower-case identifier spellings are caught too.

- **One documented home for the workspace binary list (Issue #509).**
  `rust_scorer/Cargo.toml` declares four `[[bin]]` targets, but `CONTRIBUTING.md`
  named three (omitting `gpu_pipeline_alloc_bench`) and `AGENTS.md` named two —
  each doc kept its own copy of a list the manifest owns, so every new binary
  re-opened the drift. Both now cite the README **Binaries** section (given its
  own heading so the citation resolves) instead of restating the list. New
  `scripts/check-binary-list-docs.sh` (run from `quality.sh`, covered by
  `tests/scripts/binary_list_docs.bats`) fails the gate when the README omits a
  manifest binary, or when `CONTRIBUTING.md` / `AGENTS.md` name one.

- **One documented home for the PR-summary archive (Issue #508).** The archive
  — the project's durable cross-machine memory — was split between
  `docs/pr-summary-*.md` (40 summaries, PRs 1–105) and
  `docs/archive/pr-summaries/` (110 summaries, PRs 117+) with no documented
  convention, so an agent mining prior learnings could sweep one location and
  silently miss the other. The 40 root summaries moved into
  `docs/archive/pr-summaries/`, the `.codespellrc` `skip` entry now names the
  archive path so the Issue #21 typo-fixture exemption follows the files, and
  the convention ("summaries live under `docs/archive/pr-summaries/`, one file
  per PR") is recorded in a new `docs/archive/pr-summaries/README.md` and in
  the `CONTRIBUTING.md` pull-request workflow. New
  `scripts/check-pr-summary-archive.sh` (run from `quality.sh`, covered by
  `tests/scripts/pr_summary_archive.bats`) fails the gate on a summary outside
  the archive, an uncovered codespell skip list, or a missing convention doc.

- **README "Output" section describes the shipped `gpuBackend` semantics
  (Issue #507).** The section still said the field reported which `wgpu`
  backend the scorer "would run on" (`"cpu-fallback"` "until GPU kernels
  land"), contradicting the README's own "GPU mode" section and telling
  readers GPU support had not shipped — kernels landed in Issues #82/#83/#182
  and `--gpu auto` has been the default since #83. The paragraph now states
  that the field reports the backend that **actually ran** the scoring kernel
  and names every shipped label. New `scripts/check-gpu-backend-docs.sh` (run
  from `quality.sh`, covered by `tests/scripts/gpu_backend_docs.bats`) derives
  the labels from `GpuBackendLabel::as_str` and fails the gate if the section
  loses the runtime semantics, omits a label, drops the cross-link to the GPU
  mode section, or revives the stale wording.
- **Dead `AGENTS.md` citations now point at a real home (Issue #505).** Four
  places cited `AGENTS.md` sections that never existed — a "Performance Task
  Workflow" and a "Human Escalation" section — so an agent following the
  citation found nothing. Both rules are now written down once, in
  `CONTRIBUTING.md`: the Performance Task Workflow (before/after Criterion
  evidence at the documented corpus size; a change that misses its acceptance
  bar raises no PR — post the numbers, label `negative-result`, close
  `not planned`) and Human escalation (the automation worker holds no
  `workflow` OAuth scope, so `.github/workflows/` changes need a maintainer,
  and `needs-human` always travels with an explanation comment). `README.md`,
  `docs/gpu-scoring-design.md`, `docs/performance-baseline.md` and `AGENTS.md`
  now link those anchors. New `scripts/check-docs-cross-references.sh` (run
  from `quality.sh`, covered by `tests/scripts/docs_cross_references.bats`)
  fails the gate on a dead anchor, a missing canonical section, or a document
  that re-attributes either rule to `AGENTS.md`.
- **Read-chunk docs now describe the shipped adaptive default (Issue #504).**
  The README "Large-record hosts" section still told readers the
  `NEAT_SCORER_READ_BYTES` default was a fixed 2 MiB and that production hosts
  should `export NEAT_SCORER_READ_BYTES=33554432`, contradicting both
  `AGENTS.md` and `rust_scorer/src/read_tuning.rs`, where records ≥ 8000 B
  default to 32 MiB reads. The section now documents the record-size adaptive
  default (with a Mermaid flowchart), keeps the #307 sweep as its supporting
  evidence, and states the 64 MiB `MAX_READ_BYTES` clamp.
  `docs/performance-baseline.md` keeps its dated #307 decision text unedited
  and carries an appended supersession banner. New
  `scripts/check-read-bytes-docs.sh` (run from `quality.sh`, covered by
  `tests/scripts/read_bytes_docs.bats`) reads the constants from
  `read_tuning.rs` and fails the gate if either document drifts again.

- **Acknowledged the neat-core 0.5.0 → 0.8.1 breaking bumps (Issue #252 gate).**
  neat-core removed three dead WASM/SIMD surfaces —
  `apply_derivative_simd_4way` / `derivative_batch_4way` (0.6.0),
  `apply_calculate_error_batch_4way` / `calculate_error_batch_4way` (0.7.0) and
  the unbound `get_training_state_num_*` exports (0.8.0). `rust_scorer`
  referenced none of them, so no scorer code change was required;
  `neat-core.expected-version` is bumped to `0.8.1` with the per-version
  rationale recorded inline.

### Security

- **PAT-bearing push steps hardened against in-job poisoning (Issue #497).**
  `auto-format.yml` and `version-increment.yml` run a script checked out from
  the PR head branch before the step that holds the org-level `ACTIONS_PUSH`
  PAT. That earlier step could append a `PATH` override to `$GITHUB_ENV` or
  plant `.git/hooks/pre-commit`, either of which would execute with `$GH_PAT`
  in scope. Both push steps now pin `git` and `base64` to absolute paths, pass
  `-c core.hooksPath=/dev/null` on every git invocation, and run no repository
  script (the auto-format commit message moved to a step output). Enforced by
  `scripts/check-push-step-hardening.sh` from `quality.sh` and CI. Defence in
  depth only — scoping the credential itself needs an org admin (Issue #498).

### Changed

- **The `rust_scorer` binary now links the library instead of recompiling it
  (Issue #475).** `src/main.rs` declared its own `mod` tree, so the bin target
  built a second, independent copy of every module — the shipped binary could
  drift from the code benches and tests exercise, and the crate needed 17
  `#[allow(dead_code)]` attributes to silence the unused bin-side copy. CLI
  logic moved to `src/cli.rs`; `main.rs` is a thin shim over
  `rust_scorer::cli::main`. All 17 suppressions are gone and ten `pub` items
  that existed only to dodge `dead_code` are now `pub(crate)`, so the lint is
  armed crate-wide. No behaviour or CLI-contract change.

- **neat-core baseline acknowledged at 0.5.0 (Issue #252 gate).** neat-core has
  presented three pre-1.0 breaking bumps since the recorded 0.2.5 baseline, each
  removing API `rust_scorer` does not consume: the deprecated `score_records` /
  `score_records_parallel` wrappers and `RecordBatch::PerRecord` (0.3.0),
  `PredictiveCodingEngine` (0.4.0), and the `wasm_dataset` training-data offload
  (0.5.0). scorer's migration to the flat scoring entry points had already
  landed, so no scorer code change was outstanding; verified by a clean build
  and the full test suite against neat-core 0.5.0.

### Fixed

- **Misaligned training files no longer splice records silently (Issue #476).**
  The native scorer streams every `.bin` file as one
  continuous byte stream, so the `pending` buffer carries a short tail from file
  N straight into file N+1. Any single file whose length is not a whole multiple
  of `record_bytes` therefore produced a bogus spliced record at the boundary and
  shifted every record after it — the run only complained at the very end, with
  `Trailing N bytes (incomplete record) after reading all training files`, naming
  no file, and said nothing at all when two misalignments cancelled out across the
  corpus. The WASM scorer frames records per file and asserts, so hosts that fell
  back to WASM scored a different record set from hosts on native — a small,
  systematic, one-direction score offset across the fleet. New
  `rust_scorer/src/corpus_guard.rs::assert_records_aligned` runs immediately after
  every `find_bin_files(...)` site, before any streaming, and fails loudly naming
  the offending file, its size and the remainder bytes. Zero `record_bytes` is
  rejected rather than dividing by zero, and an unreadable file is an error rather
  than a silent skip. The end-of-stream trailing-bytes checks stay as a backstop.
  No `neat-core` change.

### Removed

- **Scorer-local dead-code audit (Issue #470).** Re-ran the superseded
  `neat-core` API check across `rust_scorer/{src,tests,benches}` — **0 hits**
  for `score_records` / `score_records_parallel`; scoring still goes through
  the fused packed path (`neat_core::loss::mse_sum_batch_packed` and its
  `mae_`/`mape_` siblings). Verified every `#[allow(dead_code)]` site against
  the consumer its comment names and cleared what no longer held: the
  `env_tuning` / `read_tuning` module copies in
  `rust_scorer/src/bin/cost_scan_bench.rs` were declared but never referenced
  and have been deleted, and the now-reachable `GpuContext` and
  `score_from_creature_dir_gpu_sampled` lost their vestigial attributes (21 →
  17 sites). Also dropped the never-read `creature: &CreatureExport` parameter
  from `stream_score::accumulate_cost_sum_forward_only_fused` and its
  `_sampled` variant — `TrainingDataConfig` already carries the input/output
  widths the fused reader needs. No behaviour change.

### Changed

- **`--gpu auto` routes shallow scratch pools to GPU (Issue #467).** Issue #317
  kept every `ScratchOnly` directory pool on CPU based on the **deep**
  production shape (~1666 hidden). A creature with thousands of inputs but only
  a handful of hidden neurons — the 2461-input / 19-hidden Enceladus shape — is
  scratch-routed only because inputs count towards `num_neurons`, and it is
  **45–50 % faster on GPU** at N=50–63 on an Apple M4 Pro (`--gpu on` 2.95 s vs
  `--gpu off` 5.44 s at N=50; 3.52 s vs 7.08 s at N=63), well clear of the ≥ 3 %
  win gate. `auto` now keeps GPU for a scratch-only pool whose creatures all
  have ≤ 256 **non-input** neurons (`MAX_SHALLOW_NON_INPUT_NEURONS` /
  `directory_pool_is_shallow`) and prints no topology fallback note for them;
  deep scratch-only and mixed pools are unchanged. `auto` also now runs the
  creature-loading topology probe **once** (shared between the routing decision
  and the fallback note) instead of twice. New harness
  [`scripts/bench-shallow-gpu.sh`](./scripts/bench-shallow-gpu.sh) reproduces
  the A/B; numbers and the threshold validation sweep are in
  [`docs/performance-baseline.md`](./docs/performance-baseline.md).

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
  carrying one (the production creature has 3) falls back to CPU rather than being
  silently mis-scored. CPU↔GPU parity across all 32 point-wise squashes is asserted on Apple M4 /
  Metal by `cpu_vs_gpu_pointwise_squash_coverage`. The CPU path and the
  `auto_should_use_gpu` per-path default are unchanged; a new optional
  `BENCH_SCORING_HIDDEN_SQUASH` bench env var drives the GPU-vs-CPU A/B on the
  production squash mix.

### Changed

- **Reword private-repo mentions in the changelog and archived PR summaries to
  concept level (Issue #453).** Historical documentation named a private
  `stSoftwareAU` repository; a public repo must be self-contained, so those
  incidental mentions now describe the production creature, corpus and hosts by
  their properties instead. A new guard
  (`scripts/check-docs-private-repo-refs.sh`, wired into `quality.sh`, with BATS
  coverage in `tests/scripts/docs_private_repo_refs.bats`) keeps `CHANGELOG.md`
  and everything under `docs/` clean, completing the README (#450), source
  (#452) and automation (#451) guards. Documentation only — no code change.

- **Drop the push-to-`Develop` trigger from the CI checker workflow (Issue
  #370).** `ci.yml` runs the heavy test/lint/scan gate; as a *checker* it should
  gate the pull request, not re-run post-merge. The `push:` trigger targeting the
  default branch `Develop` duplicated the run that already gated the PR — wasting
  CI minutes and risking a red tick on `Develop` for a check that already passed.
  The workflow now fires on `pull_request` and `workflow_dispatch` only.
  Deploy/publish/release workflows are unaffected (they must keep firing on
  push). A new guard (`scripts/check-ci-push-trigger.sh`, wired into
  `quality.sh`, with BATS coverage in `tests/scripts/ci_push_trigger.bats`)
  rejects any re-added push-to-`Develop` trigger while leaving the legitimate
  `pull_request` filter on `Develop` untouched.

- **Extract the NEAT-AI-core checkout + symlink block into a local composite
  action (Issue #401).** The "checkout `stSoftwareAU/NEAT-AI-core` + symlink the
  sibling path Cargo expects" pair was copy-pasted across seven call sites in
  five workflows (`ci.yml`, `auto-format.yml`, `cargo-quality.yml`, `sbom.yml`,
  `security.yml`) and had already drifted. It now lives once in
  `.github/actions/setup-neat-core/action.yml`; every consumer calls
  `uses: ./.github/actions/setup-neat-core`, so the next path-strategy change is
  a one-file diff. The composite sets `persist-credentials: false` (least
  privilege) and opens its symlink script with `set -euo pipefail`. A new guard
  (`scripts/check-neat-core-composite-action.sh`) rejects any re-inlined copy,
  and the path-strategy, run-block-safety, and action-version-pinning guards now
  also scan `.github/actions` so the extracted block stays covered.
- **Drop the redundant `cargo check` step from the CI `quality` job (Issue
  #403).** `cargo clippy` (Run linter) drives the same rustc front-end over the
  identical `--all-targets --all-features` scope with `-D warnings`, so it is
  the strict type-check gate — the separate `Check types` step could only pass
  once clippy already had and did not reuse clippy's artefacts, adding wall-clock
  to the heaviest job for no coverage. The README "matches CI" block, its
  alignment guard (`scripts/check-readme-ci-alignment.sh`), and `AGENTS.md` are
  updated to match; the guard now rejects a re-introduced standalone
  `cargo check`.
- **Document the recommended `NEAT_SCORER_READ_BYTES` for large-record production
  hosts (Issue #307).** Swept `NEAT_SCORER_READ_BYTES` ∈ {2, 8, 16, 32, 64} MiB
  on the #296 production fixture (9848-byte records). Larger aligned reads
  recover ~20–24 % on the single-creature and multi-creature production groups
  (sweet spot 16–32 MiB) by giving the Rayon worker pool bigger per-chunk
  batches — a chunk-count amortisation effect that only helps large records.
  The global default stays **2 MiB** (the gain is record-size specific and the
  sweep host was contended, not the quiet host the merge gate requires); the
  README now documents exporting `NEAT_SCORER_READ_BYTES=33554432` (32 MiB) on
  production hosts, and `docs/performance-baseline.md` records the sweep table. No code
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
