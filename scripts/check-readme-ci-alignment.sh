#!/usr/bin/env bash
# check-readme-ci-alignment.sh — Issue #212.
#
# Guards against drift between the README "step-by-step (matches CI)" command
# block and the CI `quality` job in .github/workflows/ci.yml. A contributor
# copy-pasting the README block to "reproduce CI" must run the same gate CI
# runs — no weaker, no different.
#
# The check extracts the fenced bash block that follows the
# "Or step-by-step (matches CI):" heading in README.md, collapses line
# continuations and whitespace, and asserts every canonical CI quality command
# is present. It does NOT parse ci.yml — the canonical command list below is
# the single source of truth, kept deliberately small and explicit so a drift
# in either file is obvious in review.
#
# Usage:
#   check-readme-ci-alignment.sh [--readme PATH]
#
# Exit codes:
#   0  README block matches every canonical CI quality command
#   1  one or more commands missing, or the block could not be found
#   2  usage error

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
README="$REPO_ROOT/README.md"

while [ $# -gt 0 ]; do
  case "$1" in
    --readme)
      [ $# -ge 2 ] || { echo "Usage: $0 [--readme PATH]" >&2; exit 2; }
      README="$2"
      shift 2
      ;;
    -h|--help)
      echo "Usage: $0 [--readme PATH]"
      exit 0
      ;;
    *)
      echo "Usage: $0 [--readme PATH]" >&2
      exit 2
      ;;
  esac
done

if [ ! -f "$README" ]; then
  echo "FAIL: README not found: $README" >&2
  exit 1
fi

# Canonical CI `quality` job commands (see .github/workflows/ci.yml). Each entry
# is matched as a substring against the whitespace-collapsed README block.
# Keep this list aligned with the workflow when CI steps change.
canonical_commands=(
  'RUSTFLAGS="-D warnings"'
  'cargo deny check'
  'cargo fmt --all -- --check'
  'cargo clippy --all-targets --all-features -- -D warnings -D clippy::filter_next -D clippy::collapsible_if'
  'cargo build --workspace'
  'cargo test --workspace --all-features'
  'RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps'
)

# Extract the bash code block immediately following the "matches CI" heading.
block="$(awk '
  /Or step-by-step \(matches CI\):/ { seen = 1; next }
  seen && /^```/ { if (infence) { exit } else { infence = 1; next } }
  seen && infence { print }
' "$README")"

if [ -z "$block" ]; then
  echo "FAIL: could not find the 'matches CI' bash block in $README" >&2
  exit 1
fi

# Collapse backslash line continuations and runs of whitespace to single spaces
# so multi-line invocations (e.g. clippy) match the canonical single-line form.
collapsed="$(printf '%s\n' "$block" | tr '\n' ' ' | sed 's/\\ / /g' | tr -s ' ')"

fail=0
for cmd in "${canonical_commands[@]}"; do
  if [[ "$collapsed" == *"$cmd"* ]]; then
    echo "OK   README block includes: $cmd"
  else
    echo "FAIL README block missing CI command: $cmd" >&2
    fail=1
  fi
done

# The standalone `--workspace` clippy form is weaker than CI's invocation and
# must not be the one a contributor copies. CI runs clippy without --workspace.
if [[ "$collapsed" == *"cargo clippy --workspace"* ]]; then
  echo "FAIL README clippy uses '--workspace'; CI uses '--all-targets --all-features'" >&2
  fail=1
fi

# A standalone `cargo check` step is redundant with clippy — clippy drives the
# same rustc front-end over the same scope with `-D warnings`, so it is the
# strict type-check gate. CI dropped the separate `cargo check` step (Issue
# #403); the matches-CI block must not reintroduce it or it drifts from the job.
if [[ "$collapsed" == *"cargo check"* ]]; then
  echo "FAIL README block has a redundant 'cargo check' step; CI relies on clippy as the type-check gate (Issue #403)" >&2
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  echo "README 'matches CI' block has drifted from the CI quality job." >&2
  exit 1
fi

echo "README 'matches CI' block matches the CI quality job."
