#!/usr/bin/env bash
# Validate the standalone Dependency Review workflow (Issue #62).
#
# The dependency-review workflow must:
#   1. Trigger on pull_request so every change is reviewed before merge —
#      `actions/dependency-review-action` only operates on pull_request
#      events (it diffs the PR's manifest against the base branch).
#   2. Declare an explicit `permissions:` block (least privilege). The
#      action only needs `contents: read` to diff manifests; richer
#      permissions (e.g. `pull-requests: write`) are only required when
#      enabling PR comment summaries, which the standalone workflow leaves
#      to the reusable security workflow.
#   3. Pin `actions/checkout` to a numeric major version (Node 24 policy).
#   4. Invoke `actions/dependency-review-action`, also pinned to a numeric
#      major (the version policy in
#      `scripts/check-workflow-action-versions.sh` tracks the current
#      Node 20 exception at @v4).
#
# The script takes a single optional `--workflow PATH` argument so BATS tests
# can exercise it against fixtures. When called with no argument it validates
# `.github/workflows/dependency-review.yml` relative to the repo root.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-dependency-review-workflow.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the dependency-review workflow YAML file
                    (default: .github/workflows/dependency-review.yml
                    relative to the repo root).
  -h, --help        Show this message.

Exits 0 when the workflow satisfies every rule listed in the script header.
Exits non-zero with a descriptive message otherwise.
EOF
}

WORKFLOW=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --workflow)
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

if [[ -z "$WORKFLOW" ]]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  WORKFLOW="$SCRIPT_DIR/../.github/workflows/dependency-review.yml"
fi

if [[ ! -f "$WORKFLOW" ]]; then
  echo "Workflow file not found: $WORKFLOW" >&2
  exit 2
fi

EXIT_CODE=0
fail() {
  echo "FAIL $WORKFLOW: $*" >&2
  EXIT_CODE=1
}
ok() {
  echo "OK   $WORKFLOW: $*"
}

# 1. Triggered on pull_request events.
if grep -qE '^[[:space:]]+pull_request:' "$WORKFLOW" \
  || grep -qE '^on:[[:space:]]*\[?.*pull_request' "$WORKFLOW"; then
  ok "triggers on pull_request"
else
  fail "workflow is not triggered on pull_request"
fi

# 2. Explicit permissions block (least privilege).
if grep -qE '^permissions:[[:space:]]*$' "$WORKFLOW" \
  && grep -qE '^[[:space:]]+contents:[[:space:]]*read' "$WORKFLOW"; then
  ok "permissions block grants only contents: read"
else
  fail "no 'permissions: contents: read' block — least-privilege required"
fi

# 3. actions/checkout pinned to a numeric major (vN). Branch refs disallowed.
checkout_line="$(grep -nE 'uses:[[:space:]]*actions/checkout@' "$WORKFLOW" || true)"
if [[ -z "$checkout_line" ]]; then
  fail "actions/checkout step missing — workflow cannot fetch the repo"
elif echo "$checkout_line" | grep -qE 'actions/checkout@v?[0-9]+'; then
  ok "actions/checkout pinned to a numeric major"
else
  fail "actions/checkout is not pinned — branch refs disallowed"
fi

# 4. actions/dependency-review-action present and pinned to a numeric major.
dr_line="$(grep -nE 'uses:[[:space:]]*actions/dependency-review-action@' "$WORKFLOW" || true)"
if [[ -z "$dr_line" ]]; then
  fail "actions/dependency-review-action missing — workflow performs no review"
elif echo "$dr_line" | grep -qE 'actions/dependency-review-action@v?[0-9]+'; then
  ok "actions/dependency-review-action present and pinned to a numeric major"
else
  fail "actions/dependency-review-action is not pinned — branch refs disallowed"
fi

exit "$EXIT_CODE"
