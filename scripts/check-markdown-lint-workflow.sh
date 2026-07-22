#!/usr/bin/env bash
# Validate the standalone Markdown Lint workflow (Issue #63).
#
# The markdown-lint workflow must:
#   1. Trigger on pull_request so every change is linted before merge.
#   2. Declare an explicit `permissions:` block (least privilege —
#      `contents: read` is sufficient because markdownlint only reads source).
#   3. Pin `actions/checkout` to a numeric major version (Node 24 policy —
#      see scripts/check-workflow-action-versions.sh).
#   4. Provision Node via `actions/setup-node` pinned to a numeric major.
#   5. Install and invoke `markdownlint-cli2` so the lint gate actually runs.
#
# The script takes a single optional `--workflow PATH` argument so BATS tests
# can exercise it against fixtures. When called with no argument it validates
# `.github/workflows/markdown-lint.yml` relative to the repo root.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-markdown-lint-workflow.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the markdown-lint workflow YAML file (default:
                    .github/workflows/markdown-lint.yml relative to the repo
                    root).
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
  WORKFLOW="$SCRIPT_DIR/../.github/workflows/markdown-lint.yml"
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

# 4. actions/setup-node pinned to a numeric major (vN). Branch refs disallowed.
setup_node_line="$(grep -nE 'uses:[[:space:]]*actions/setup-node@' "$WORKFLOW" || true)"
if [[ -z "$setup_node_line" ]]; then
  fail "actions/setup-node step missing — Node toolchain not provisioned"
elif echo "$setup_node_line" | grep -qE 'actions/setup-node@v?[0-9]+'; then
  ok "actions/setup-node pinned to a numeric major"
else
  fail "actions/setup-node is not pinned — branch refs disallowed"
fi

# 5. markdownlint-cli2 must be installed AND invoked. Two separate checks so
#    a missing install or a missing run surfaces as a distinct failure.
if grep -qE 'npm[[:space:]]+install[[:space:]].*markdownlint-cli2' "$WORKFLOW"; then
  ok "markdownlint-cli2 install step present"
else
  fail "markdownlint-cli2 install step missing — gate cannot run without the binary"
fi

if grep -qE '^[[:space:]]+(- )?run:[[:space:]]*markdownlint-cli2' "$WORKFLOW"; then
  ok "markdownlint-cli2 invoked"
else
  fail "markdownlint-cli2 is not invoked — no 'run: markdownlint-cli2' step found"
fi

# 6. A lint/checker workflow must gate the PR only — it must NOT trigger on
#    push to the default branch `Develop` (Issue #371, reversing Issue #207).
#    Once this check is a required status, a post-merge push run only duplicates
#    the run that already gated the PR: it wastes CI minutes and can leave a red
#    tick on the default branch for a check that already passed. The PR gate
#    (rule 1) is the sole gate a checker needs.
#      * No `push:` trigger              -> pass (PR-gated only).
#      * `push:` whose branches exclude   -> pass (non-default pushes are fine).
#        the default branch `Develop`
#      * `push:` reaching `Develop`, or   -> fail.
#        an unfiltered `push:`
if grep -qE '^[[:space:]]+push:' "$WORKFLOW"; then
  push_branches="$(awk '
    /^[[:space:]]+push:[[:space:]]*$/ { in_push = 1; next }
    in_push && /branches:/ { print; exit }
    in_push && /^[^[:space:]#]/ { exit }
  ' "$WORKFLOW")"
  if [[ -z "$push_branches" ]]; then
    fail "push trigger has an unfiltered branches list — a lint/checker workflow must not push to the default branch (Develop); remove push or exclude Develop (Issue #371)"
  elif echo "$push_branches" | grep -q 'Develop'; then
    fail "push trigger targets the default branch (Develop) — a lint/checker workflow should gate the PR only (Issue #371)"
  else
    ok "push trigger excludes the default branch (Develop)"
  fi
else
  ok "no push trigger — the lint workflow gates the PR only (Issue #371)"
fi

exit "$EXIT_CODE"
