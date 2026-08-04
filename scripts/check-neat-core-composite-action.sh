#!/usr/bin/env bash
# Guard: the NEAT-AI-core checkout + sibling symlink block lives in ONE local
# composite action, not copy-pasted across workflows (Issue #401).
#
# The "checkout stSoftwareAU/NEAT-AI-core + symlink to the sibling path Cargo
# expects" block was previously duplicated across seven call sites in five
# workflow files. The copies drifted (one variant opened the symlink script with
# `set -euo pipefail`, others did not), which is exactly the failure mode
# duplication invites. This guard keeps the block a single source of truth:
#
#   1. The composite action file exists and declares `using: composite`.
#   2. It contains the NEAT-AI-core checkout (in-workspace `path:`) AND the
#      sibling-link step whose `run:` block opens with `set -euo pipefail`.
#   3. No workflow under .github/workflows/ still inlines a
#      `repository: stSoftwareAU/NEAT-AI-core` checkout — every consumer must go
#      through the composite.
#   4. At least one workflow references the composite via
#      `uses: ./.github/actions/setup-neat-core`.
#
# Designed for reuse from BATS tests and quality.sh.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/check-harness.sh
source "$SCRIPT_DIR/lib/check-harness.sh"

usage() {
  cat <<'EOF'
Usage: check-neat-core-composite-action.sh [--action PATH] [--workflows DIR]

Options:
  --action PATH     Path to the composite action YAML (default:
                    .github/actions/setup-neat-core/action.yml).
  --workflows DIR   Directory containing workflow YAML files (default:
                    .github/workflows relative to the repo root).
  -h, --help        Show this message.

Exits 0 when the composite action is present and valid AND no workflow inlines a
NEAT-AI-core checkout. Exits non-zero with a descriptive message otherwise.
EOF
}

ACTION=""
WORKFLOWS_DIR=""
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

# Every message names its own file, so ok/fail carry no subject prefix.

# --- 1 & 2: the composite action exists and is well formed. -----------------
if [[ ! -f "$ACTION" ]]; then
  fail "composite action not found: $ACTION — the shared NEAT-AI-core setup must live here (Issue #401)."
else
  if grep -Eq '^[[:space:]]*using:[[:space:]]*composite[[:space:]]*$' "$ACTION"; then
    ok "composite action present: $ACTION"
  else
    fail "$ACTION: not a composite action (missing 'using: composite')."
  fi

  if grep -q 'repository:[[:space:]]*stSoftwareAU/NEAT-AI-core' "$ACTION"; then
    ok "composite checks out stSoftwareAU/NEAT-AI-core"
  else
    fail "$ACTION: does not check out stSoftwareAU/NEAT-AI-core."
  fi

  if grep -q 'Link NEAT-AI-core sibling path' "$ACTION"; then
    ok "composite has the sibling-link step"
  else
    fail "$ACTION: missing the 'Link NEAT-AI-core sibling path' step."
  fi

  # The symlink run block must open with set -euo pipefail — reuse the shared
  # run-block safety guard so this stays a single source of truth.
  if "$SCRIPT_DIR/check-run-block-safety.sh" --workflows "$(dirname "$ACTION")" >/dev/null 2>&1; then
    ok "composite symlink run block opens with 'set -euo pipefail'"
  else
    fail "$ACTION: symlink run block does not open with 'set -euo pipefail' (Issue #400)."
  fi
fi

# --- 3 & 4: workflows go through the composite, none inline the checkout. ----
check_require_dir "$WORKFLOWS_DIR"

inline_hits="$(grep -rln 'repository:[[:space:]]*stSoftwareAU/NEAT-AI-core' \
  --include='*.yml' --include='*.yaml' "$WORKFLOWS_DIR" || true)"
if [[ -n "$inline_hits" ]]; then
  while IFS= read -r wf; do
    [[ -n "$wf" ]] || continue
    fail "$wf: inlines a NEAT-AI-core checkout — replace it with 'uses: ./.github/actions/setup-neat-core' (Issue #401)."
  done <<<"$inline_hits"
else
  ok "no workflow inlines a NEAT-AI-core checkout"
fi

consumer_hits="$(grep -rl 'uses:[[:space:]]*\./\.github/actions/setup-neat-core' \
  --include='*.yml' --include='*.yaml' "$WORKFLOWS_DIR" || true)"
if [[ -n "$consumer_hits" ]]; then
  ok "at least one workflow references the composite action"
else
  fail "no workflow references 'uses: ./.github/actions/setup-neat-core' — the composite is unused (Issue #401)."
fi

exit "$EXIT_CODE"
