#!/usr/bin/env bash
# check-readme-ci-alignment.sh — Issues #212, #506.
#
# Guards against drift between the README's CI documentation and the workflows
# it describes. Three invariants:
#
#   1. (Issue #212) The "step-by-step (matches CI)" command block must run the
#      same gate the CI `quality` job runs — no weaker, no different. The check
#      extracts the fenced bash block that follows the
#      "Or step-by-step (matches CI):" heading, collapses line continuations and
#      whitespace, and asserts every canonical CI quality command is present. It
#      does NOT parse ci.yml — the canonical command list below is the single
#      source of truth, kept deliberately small and explicit so a drift in
#      either file is obvious in review.
#   2. (Issue #506) The README must not present `ludeeus/action-shellcheck` as
#      part of the supply chain once no workflow invokes that wrapper. PR #184
#      switched `ci.yml`'s `shell-checks` job to the runner's pre-installed
#      `shellcheck` binary; a README still naming the wrapper sends a security
#      reviewer to audit third-party code that no longer executes.
#   3. (Issue #506) Every `owner/action@vN` reference in the README must be at
#      or above the major that `scripts/check-workflow-action-versions.sh`
#      requires. Understating a major (e.g. calling
#      `actions/dependency-review-action@v4` a tracked Node 20 exception when
#      the validator requires the Node 24 v5 line) misrepresents the repo's
#      actual runtime posture.
#
# Usage:
#   check-readme-ci-alignment.sh [--readme PATH] [--workflows DIR]
#                                [--versions-script PATH]
#
# Exit codes:
#   0  README matches the workflows it documents
#   1  one or more invariants violated, or the matches-CI block was not found
#   2  usage error

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
README="$REPO_ROOT/README.md"
WORKFLOWS_DIR="$REPO_ROOT/.github/workflows"
VERSIONS_SCRIPT="$SCRIPT_DIR/check-workflow-action-versions.sh"

USAGE="Usage: $0 [--readme PATH] [--workflows DIR] [--versions-script PATH]"

while [ $# -gt 0 ]; do
  case "$1" in
    --readme)
      [ $# -ge 2 ] || { echo "$USAGE" >&2; exit 2; }
      README="$2"
      shift 2
      ;;
    --workflows)
      [ $# -ge 2 ] || { echo "$USAGE" >&2; exit 2; }
      WORKFLOWS_DIR="$2"
      shift 2
      ;;
    --versions-script)
      [ $# -ge 2 ] || { echo "$USAGE" >&2; exit 2; }
      VERSIONS_SCRIPT="$2"
      shift 2
      ;;
    -h|--help)
      echo "$USAGE"
      exit 0
      ;;
    *)
      echo "$USAGE" >&2
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

# --- Issue #506: the README must not advertise a wrapper action no workflow
# runs. `ci.yml`'s shell-checks job calls the runner's pre-installed
# `shellcheck` binary (PR #184), so `ludeeus/action-shellcheck` is out of the
# supply chain and must not be described as still in it.
if [ -d "$WORKFLOWS_DIR" ]; then
  if grep -rqE 'uses:[[:space:]]*ludeeus/action-shellcheck@' "$WORKFLOWS_DIR"; then
    echo "OK   ludeeus/action-shellcheck is still used by a workflow"
  elif grep -q 'ludeeus/action-shellcheck' "$README"; then
    echo "FAIL README names ludeeus/action-shellcheck, but no workflow invokes that wrapper (Issue #506)" >&2
    fail=1
  else
    echo "OK   README does not claim the removed ludeeus/action-shellcheck wrapper"
  fi
else
  echo "FAIL workflows directory not found: $WORKFLOWS_DIR" >&2
  fail=1
fi

# --- Issue #506: every `owner/action@vN` the README names must be at or above
# the major floor that check-workflow-action-versions.sh enforces. The
# validator's `required:N` table is the single source of truth for the Node 24
# policy, so a README naming an older major (e.g. a stale Node 20 exception)
# understates the runtime the workflows actually pin.
required_major_for() {
  local action="$1" escaped line
  escaped="${action//./\\.}"
  line="$(grep -E "^[[:space:]]*${escaped}\)[[:space:]]*echo \"required:[0-9]+\"" "$VERSIONS_SCRIPT" || true)"
  [ -n "$line" ] || return 1
  printf '%s\n' "$line" | sed -E 's/.*required:([0-9]+).*/\1/' | head -n 1
}

if [ ! -f "$VERSIONS_SCRIPT" ]; then
  echo "FAIL action-version validator not found: $VERSIONS_SCRIPT" >&2
  fail=1
else
  while IFS= read -r ref; do
    [ -n "$ref" ] || continue
    action="${ref%%@*}"
    version="${ref#*@}"
    major="${version#v}"
    major="${major%%.*}"
    floor="$(required_major_for "$action")" || continue
    if [ "$major" -lt "$floor" ]; then
      echo "FAIL README names $ref but $action requires major >= $floor (Issue #506)" >&2
      fail=1
    else
      echo "OK   README $ref satisfies the >= v$floor floor"
    fi
  # Only human-readable version refs (`@vN`, `@vN.N.N`, `@N.N.N`) are checked.
  # A 40-character SHA pin never starts with `v` and never contains a dot, so
  # this alternation cannot mistake `@5f6978fa…` for "major 5".
  done < <(grep -oE '[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+@(v[0-9]+(\.[0-9]+)*|[0-9]+(\.[0-9]+)+)' \
    "$README" | sort -u)
fi

if [ "$fail" -ne 0 ]; then
  echo "README CI documentation has drifted from the workflows it describes." >&2
  exit 1
fi

echo "README 'matches CI' block matches the CI quality job."
