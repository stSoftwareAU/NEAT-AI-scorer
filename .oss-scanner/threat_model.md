# Threat model

Reporting channels and the supported branch are in
[SECURITY.md](../SECURITY.md); the CLI contract is in the
[README](../README.md#cli). This file is the short brief for the scanner.

## What this project does and where untrusted input enters

`rust_scorer` is a native CLI that scores NEAT-AI creatures against a
fixed-width binary training corpus and prints one JSON result to stdout. It
reads, never writes, the paths it is given. Creatures arrive from other fleet
machines and the corpus is large and machine-generated, so treat as untrusted:

- creature JSON — `<creature.json>`, every `*.json` in a creatures directory,
  or stdin under `--creature-stdin` (neuron counts, synapse endpoints and
  roles, squash ids, weights, declared `input`/`output` widths);
- the `.bin` files in `<training_data_dir>`: little-endian `f32` records whose
  width is derived from the creature, possibly misaligned or truncated;
- the line-delimited verdicts read from stdin under `--race-stdio`;
- command-line arguments (`--cost`, `--sample-rate`, `--record-bytes`, …).

The `NEAT_SCORER_*` environment knobs are operator settings and trusted.

## Components that matter most / least

Most important:

- the record stream and its `unsafe` bulk unpack (`rust_scorer/src/stream_io.rs`),
  the alignment guard (`corpus_guard.rs`) and the scoring loops
  (`stream_score.rs`, `multi_score.rs`, `scoring.rs`, `cost.rs`);
- the GPU host code that sizes and fills buffers for the WGSL kernels
  (`rust_scorer/src/gpu/`, `rust_scorer/src/shaders/`): an out-of-bounds index
  or a mis-sized buffer computed from creature or corpus data;
- creature parsing and compilation, which happen in the
  [`neat-core`](https://github.com/stSoftwareAU/NEAT-AI-core) dependency but are
  reached here first; report the root cause against that repository as well;
- the `--race-stdio` protocol parser (`racing_stdio.rs`) and the signal
  handler (`signal_exit.rs`, `unsafe` libc calls).

Lower priority: the bench binaries under `rust_scorer/src/bin/`, the test
fixtures (`*_fixture.rs`), `--host-report`, and `scripts/`, which are
maintainer tooling.

## How to exercise it

From `/src`: `cargo test --workspace --all-features -- --test-threads=2` runs
the suite; `rust_scorer/tests/fixtures/` holds small creatures and an
`identity_data.bin` corpus. Run the CLI as
`target/debug/rust_scorer <creature.json|dir> <data_dir>`. The image ships
Mesa's lavapipe, a software Vulkan adapter, so `--gpu on` exercises the GPU
kernels on the CPU; `--gpu off` forces the CPU pipeline.

## How you rate severity

- High or above: memory-safety faults (out-of-bounds read or write,
  use-after-free, uninitialised memory) in `unsafe` code or GPU buffer
  handling reachable from a creature or corpus; critical if attacker-controlled.
- Medium: a panic, abort, unbounded allocation or hang on malformed creature
  JSON, corpus files or `--race-stdio` input; a silently wrong score (a
  spliced or skipped record, a misread synapse role) is also medium, because
  fitness drives which creatures the fleet keeps.
- Low: slow paths that need operator-chosen environment knobs, and crashes in
  bench binaries or tests.

## Anything to leave alone

- GPU device loss mid-run is handled by design (`gpu/device_loss.rs` falls
  back to CPU under `--gpu auto`); report a wrong fallback, not the loss.
- `target-cpu=native` in `.cargo/config.toml` is deliberate fleet tuning.
- Large resident read buffers sized by the self-tuning code are expected; see
  [docs/self-tuning.md](../docs/self-tuning.md).
