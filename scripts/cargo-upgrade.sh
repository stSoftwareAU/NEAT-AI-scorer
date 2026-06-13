#!/bin/bash
# Opt-in dependency upgrade step for ./quality.sh (Issue #210).
#
# By DEFAULT this is a no-op. The local quality gate must stay read-only
# against Cargo.lock / Cargo.toml so a contributor running the gate to
# validate an unrelated change never ends up with surprise dependency
# bumps in their working tree. Routine bumps have a dedicated,
# quarantine-gated path in ./bump-deps.sh (Issue #105).
#
# Opt in with the `--upgrade` flag or QUALITY_UPGRADE=1, e.g.:
#   ./quality.sh --upgrade
#   QUALITY_UPGRADE=1 ./quality.sh
#   ./scripts/cargo-upgrade.sh --upgrade
set -euo pipefail

want_upgrade=0
if [[ "${QUALITY_UPGRADE:-0}" == "1" ]]; then
  want_upgrade=1
fi
for arg in "$@"; do
  case "$arg" in
    --upgrade) want_upgrade=1 ;;
  esac
done

if [[ "$want_upgrade" -ne 1 ]]; then
  echo "⏭️  Skipping dependency upgrade — read-only gate."
  echo "   Opt in with: ./quality.sh --upgrade  (or QUALITY_UPGRADE=1 ./quality.sh)"
  echo "   Routine bumps go through ./bump-deps.sh (quarantine-gated, Issue #105)."
  exit 0
fi

echo "📦 Upgrading Rust library dependencies (opt-in)..."
if command -v cargo-upgrade &>/dev/null; then
  cargo upgrade --incompatible
  cargo update
else
  echo "⚠️  cargo-edit not installed — skipping dependency upgrade"
  echo "   Install with: cargo install cargo-edit"
fi
