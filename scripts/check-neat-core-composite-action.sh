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
      [[ -n "$ACTION" ]] || { echo "--action requires a path argument" >&2; exit 2; }
      shift 2
      ;;
    --workflows)
      WORKFLOWS_DIR="${2:-}"
      [[ -n "$WORKFLOWS_DIR" ]] || { echo "--workflows requires a directory argument" >&2; exit 2; }
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[[ -n "$ACTION" ]] || ACTION="$SCRIPT_DIR/../.github/actions/setup-neat-core/action.yml"
[[ -n "$WORKFLOWS_DIR" ]] || WORKFLOWS_DIR="$SCRIPT_DIR/../.github/workflows"

EXIT_CODE=0
fail() { echo "FAIL $*" >&2; EXIT_CODE=1; }
ok() { echo "OK   $*"; }

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
if [[ ! -d "$WORKFLOWS_DIR" ]]; then
  echo "Workflows directory not found: $WORKFLOWS_DIR" >&2
  exit 2
fi

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
