#!/usr/bin/env bash
# Guard: the NEAT-AI-core sibling checkout stays RETIRED (Issues #401, #630).
#
# `neat-core` used to be an unpinned `path` dependency on a sibling
# NEAT-AI-core clone, so every workflow that touched Cargo had to check that
# sibling out and symlink it. That block lived in one local composite action
# (`.github/actions/setup-neat-core`, Issue #401) to stop the copies drifting.
#
# Issue #630 pinned `neat-core` to a NEAT-AI-core RELEASE TAG
# (`rust_scorer/Cargo.toml`), which Cargo fetches itself — no sibling checkout
# resolves anything any more, so both the composite action and every inline
# checkout of NEAT-AI-core are retired. This guard keeps them retired:
#
#   1. The composite action is gone — a workflow that `uses:` it would fail
#      with "can't find action.yml", so a resurrected reference is caught here
#      instead of in a red CI run.
#   2. No workflow references `uses: ./.github/actions/setup-neat-core`.
#   3. No workflow (or local action) inlines a
#      `repository: stSoftwareAU/NEAT-AI-core` checkout — the pin makes the
#      clone dead weight, and a stale clone beside a pinned build is worse than
#      no clone at all: it looks like the source being compiled and is not.
#   4. The pin the retirement rests on is actually declared: the crate manifest
#      carries a `git`+`tag` NEAT-AI-core dependency, not a `path` one.
#
# Designed for reuse from BATS tests and quality.sh.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/check-harness.sh
source "$SCRIPT_DIR/lib/check-harness.sh"

usage() {
  cat <<'EOF'
Usage: check-neat-core-composite-action.sh [--action PATH] [--workflows DIR]
                                           [--manifest PATH]

Options:
  --action PATH     Path the retired composite action must NOT occupy
                    (default: .github/actions/setup-neat-core/action.yml).
  --workflows DIR   Directory containing workflow YAML files (default:
                    .github/workflows relative to the repo root).
  --manifest PATH   Crate manifest carrying the neat-core pin (default:
                    rust_scorer/Cargo.toml relative to the repo root).
  -h, --help        Show this message.

Exits 0 when the retired composite action is absent, no workflow checks out
NEAT-AI-core, and the crate manifest pins neat-core to a release tag. Exits
non-zero with a descriptive message otherwise.
EOF
}

ACTION=""
WORKFLOWS_DIR=""
MANIFEST=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --action)
      ACTION="${2:-}"
      [[ -n "$ACTION" ]] || check_die 2 "--action requires a path argument"
      shift 2
      ;;
    --workflows)
      WORKFLOWS_DIR="${2:-}"
      [[ -n "$WORKFLOWS_DIR" ]] || check_die 2 "--workflows requires a directory argument"
      shift 2
      ;;
    --manifest)
      MANIFEST="${2:-}"
      [[ -n "$MANIFEST" ]] || check_die 2 "--manifest requires a path argument"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      check_unknown_arg "$1"
      ;;
  esac
done

[[ -n "$ACTION" ]] || ACTION="$(check_repo_path ".github/actions/setup-neat-core/action.yml")"
[[ -n "$WORKFLOWS_DIR" ]] || WORKFLOWS_DIR="$(check_repo_path ".github/workflows")"
[[ -n "$MANIFEST" ]] || MANIFEST="$(check_repo_path "rust_scorer/Cargo.toml")"

# Every message names its own file, so ok/fail carry no subject prefix.

# --- 1: the composite action is retired. ------------------------------------
if [[ -e "$ACTION" ]]; then
  fail "$ACTION: the setup-neat-core composite action is retired — neat-core is pinned to a release tag and needs no sibling checkout (Issue #630)."
else
  ok "retired composite action absent: $ACTION"
fi

# --- 2 & 3: no workflow reaches for the sibling clone. ----------------------
check_require_dir "$WORKFLOWS_DIR"

consumer_hits="$(grep -rln 'uses:[[:space:]]*\./\.github/actions/setup-neat-core' \
  --include='*.yml' --include='*.yaml' "$WORKFLOWS_DIR" || true)"
if [[ -n "$consumer_hits" ]]; then
  while IFS= read -r wf; do
    [[ -n "$wf" ]] || continue
    fail "$wf: references the retired './.github/actions/setup-neat-core' composite — delete the step; the pinned neat-core needs no sibling checkout (Issue #630)."
  done <<<"$consumer_hits"
else
  ok "no workflow references the retired composite action"
fi

inline_hits="$(grep -rln 'repository:[[:space:]]*stSoftwareAU/NEAT-AI-core' \
  --include='*.yml' --include='*.yaml' "$WORKFLOWS_DIR" || true)"
if [[ -n "$inline_hits" ]]; then
  while IFS= read -r wf; do
    [[ -n "$wf" ]] || continue
    fail "$wf: checks out stSoftwareAU/NEAT-AI-core — the pinned release tag in rust_scorer/Cargo.toml is what the build resolves, so the clone is dead weight (Issue #630)."
  done <<<"$inline_hits"
else
  ok "no workflow checks out stSoftwareAU/NEAT-AI-core"
fi

# --- 4: the pin the retirement rests on. ------------------------------------
check_require_file "$MANIFEST"
if grep -qE '^[[:space:]]*neat-core[[:space:]]*=.*path[[:space:]]*=' "$MANIFEST"; then
  fail "$MANIFEST: neat-core is declared as a path dependency again — the release-tag pin is what removes the sibling checkout (Issue #630)."
elif grep -qE '^[[:space:]]*neat-core[[:space:]]*=.*git[[:space:]]*=.*tag[[:space:]]*=[[:space:]]*"v[0-9]+\.[0-9]+\.[0-9]+"' "$MANIFEST"; then
  ok "$MANIFEST: neat-core is pinned to a NEAT-AI-core release tag"
else
  fail "$MANIFEST: no 'neat-core = { git = …, tag = \"v<semver>\" }' pin found — scripts/family-pins.sh has nothing to move (Issue #630)."
fi

exit "$EXIT_CODE"
