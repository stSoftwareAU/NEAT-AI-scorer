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
#   BENCH_FUSED_FILES     .bin shards for the `fused_multi_file` parallel-read
#                         group (default 26 — production's file count, #529)
#
# Production creature bench (Issue #296) — `production_single_creature` /
# `production_multi_creature`. This public repo ships no production creature and
# **fetches nothing** at bench time (Issue #448): supply your own local
# `network.json` via BENCH_PROD_CREATURE and the bench builds a synthetic corpus
# sized to match it. When BENCH_PROD_CREATURE is unset the production benches
# skip cleanly. Once a local creature is supplied the bench is fail-loud — it
# panics (never falls back to the synthetic fixture) if the creature cannot be
# read / deserialized or is not production-sized.
#   BENCH_PROD_CREATURE   path to a local network.json (required to run the
#                         production benches; unset skips them — nothing fetched)
#   BENCH_PROD_BYTES      total bytes per production .bin corpus (default 64 MiB;
#                         the full production corpus is 20 845 703 976 bytes /
#                         520 files — see docs/performance-baseline.md)
#   BENCH_PROD_CREATURES  creature copies for the multi-creature group (default 4)
#
# All extra arguments are forwarded to `cargo bench`. Common filters:
#   ./scripts/run-benches.sh -- score_from_json_fused
#   ./scripts/run-benches.sh -- score_from_creature_dir/creatures/10
#   ./scripts/run-benches.sh -- production_          # production creature only
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
echo "   BENCH_FUSED_FILES=${BENCH_FUSED_FILES:-26 (default)}"
echo "   BENCH_PROD_CREATURE=${BENCH_PROD_CREATURE:-<unset — production benches skipped; supply a local network.json>}"
echo "   BENCH_PROD_BYTES=${BENCH_PROD_BYTES:-67108864 (default 64 MiB)}"
echo "   BENCH_PROD_CREATURES=${BENCH_PROD_CREATURES:-4 (default)}"
echo

cargo bench -p rust_scorer "$@"
