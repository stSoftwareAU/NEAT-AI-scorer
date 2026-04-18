#!/usr/bin/env bash
# Temporary local runner — tune I/O with env vars (same as `rust_scorer` / `float_scan_bench`).
set -euo pipefail

cd "$(dirname "$0")"

cargo build --release -p rust_scorer

# Defaults: pipelined double-buffer, ~2 MiB read target (rounded to whole records inside the scorer).
# Try A/B (examples — uncomment one block at a time):
#
# export NEAT_SCORER_IO_MODE=single
# export NEAT_SCORER_READ_BYTES=2097152
#
# export NEAT_SCORER_IO_MODE=double
# export NEAT_SCORER_READ_BYTES=8388608
#
# export NEAT_SCORER_IO_MODE=single
# export NEAT_SCORER_READ_BYTES=8388608

# Optional: parallel forward-only activation (each worker = full `CompiledNetwork` clone).
export NEAT_SCORER_ACTIVATION_THREADS=8

./target/release/rust_scorer ../GRQ-cluster/network.json ../GRQ/.trainData-binary_115
