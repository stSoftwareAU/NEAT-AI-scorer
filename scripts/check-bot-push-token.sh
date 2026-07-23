#!/usr/bin/env bash
# Validate that bot-push PR workflows authenticate with ACTIONS_PUSH
# (Issue #435).
#
# Auto Format and Version Increment commit back to the PR branch. A push
# authenticated only with GITHUB_TOKEN attributes the resulting
# `synchronize` event to github-actions[bot], and GitHub holds the
# follow-on required checks behind "N checks awaiting approval /
# Approve and run". NEAT-AI avoids this by pushing with the org-level
# ACTIONS_PUSH PAT (GITHUB_TOKEN fallback when unset).
#
# Rule: each guarded workflow must reference
# `secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN`.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-bot-push-token.sh [--workflow PATH]

Options:
  --workflow PATH   Validate a single workflow YAML file. When omitted,
                    both auto-format.yml and version-increment.yml under
                    .github/workflows/ are checked.
  -h, --help        Show this message.

Exits 0 when every target workflow authenticates bot pushes with
ACTIONS_PUSH (GITHUB_TOKEN fallback). Exits non-zero otherwise.
EOF
}

WORKFLOW=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --workflow)
      if [[ $# -lt 2 ]]; then
        echo "Missing value for --workflow" >&2
        usage >&2
        exit 2
      fi
      WORKFLOW="$2"
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

WORKFLOWS=()
if [[ -n "$WORKFLOW" ]]; then
  WORKFLOWS=("$WORKFLOW")
else
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  WORKFLOWS=(
    "$SCRIPT_DIR/../.github/workflows/auto-format.yml"
    "$SCRIPT_DIR/../.github/workflows/version-increment.yml"
  )
fi

EXIT_CODE=0

validate_workflow() {
  local wf="$1"

  if [[ ! -f "$wf" ]]; then
    echo "Workflow file not found: $wf" >&2
    EXIT_CODE=2
    return
  fi

  if grep -qE 'secrets\.ACTIONS_PUSH[[:space:]]*\|\|[[:space:]]*secrets\.GITHUB_TOKEN' "$wf"; then
    echo "OK   $wf: push authenticates with ACTIONS_PUSH (GITHUB_TOKEN fallback)"
  else
    echo "FAIL $wf: no 'secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN' — bot pushes will gate PR checks behind Approve and run (Issue #435)" >&2
    EXIT_CODE=1
  fi
}

for wf in "${WORKFLOWS[@]}"; do
  validate_workflow "$wf"
done

exit "$EXIT_CODE"
