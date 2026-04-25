#!/bin/bash
# scripts/run-benches.sh — Issue #36
#
# Reproduce the Criterion bench suite documented in
# `docs/performance-baseline.md`. Not wired into `quality.sh`: Criterion runs
# are slow and not deterministic enough to gate every commit, so this is the
# manual entry point contributors use when establishing or refreshing a
# baseline.
#
# Usage:
#   ./scripts/run-benches.sh            # default (small fixture, ~minutes)
#   BENCH_SCORING_BYTES=200000000 \
#     ./scripts/run-benches.sh          # ~200 MB corpus (issue acceptance criterion)
#
# Tuning environment variables (see `rust_scorer/benches/scoring.rs`):
#   BENCH_SCORING_BYTES   total bytes per .bin corpus (default 16 MiB)
#   BENCH_SCORING_INPUTS  inputs per record (default 8)
#   BENCH_SCORING_OUTPUTS outputs per record (default 2)
#   BENCH_SCORING_HIDDEN  hidden neurons per synthetic creature (default 8)
#
# All extra arguments are forwarded to `cargo bench`. Common filters:
#   ./scripts/run-benches.sh -- score_from_json_fused
#   ./scripts/run-benches.sh -- score_from_creature_dir/creatures/10
set -euo pipefail

if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi

cd "$(dirname "$0")/.."

echo "🏁 Running rust_scorer Criterion benches"
echo "   BENCH_SCORING_BYTES=${BENCH_SCORING_BYTES:-16777216 (default)}"
echo "   BENCH_SCORING_INPUTS=${BENCH_SCORING_INPUTS:-8 (default)}"
echo "   BENCH_SCORING_OUTPUTS=${BENCH_SCORING_OUTPUTS:-2 (default)}"
echo "   BENCH_SCORING_HIDDEN=${BENCH_SCORING_HIDDEN:-8 (default)}"
echo

cargo bench -p rust_scorer "$@"
