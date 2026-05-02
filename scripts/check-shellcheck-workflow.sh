#!/usr/bin/env bash
# Validate the standalone ShellCheck Lint workflow (Issue #67).
#
# The shellcheck workflow must:
#   1. Trigger on pull_request so every change is shell-linted before merge.
#   2. Declare an explicit `permissions:` block (least privilege —
#      `contents: read` is sufficient because shellcheck only reads source).
#   3. Pin `actions/checkout` to a numeric major version (Node 24 policy —
#      see scripts/check-workflow-action-versions.sh).
#   4. Invoke ShellCheck via `ludeeus/action-shellcheck` pinned to a numeric
#      release (NOT `@master` — branch refs are forbidden by Issue #24
#      because a compromised upstream commit would silently alter CI
#      behaviour).
#   5. Declare an explicit `severity:` value so the gate is deterministic
#      across upstream default changes.
#
# The script takes a single optional `--workflow PATH` argument so BATS tests
# can exercise it against fixtures. When called with no argument it validates
# `.github/workflows/shellcheck.yml` relative to the repo root.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-shellcheck-workflow.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the shellcheck workflow YAML file (default:
                    .github/workflows/shellcheck.yml relative to the repo
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
  WORKFLOW="$SCRIPT_DIR/../.github/workflows/shellcheck.yml"
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

# 4. ludeeus/action-shellcheck pinned to a numeric version. The suggested
#    `@master` ref in the issue template is rejected here — supply-chain
#    hygiene (Issue #24) requires a tagged release.
shellcheck_line="$(grep -nE 'uses:[[:space:]]*ludeeus/action-shellcheck@' "$WORKFLOW" || true)"
if [[ -z "$shellcheck_line" ]]; then
  fail "ludeeus/action-shellcheck step missing — shellcheck is not invoked"
elif echo "$shellcheck_line" | grep -qE 'ludeeus/action-shellcheck@v?[0-9]+'; then
  ok "ludeeus/action-shellcheck pinned to a numeric release"
else
  fail "ludeeus/action-shellcheck is not pinned — @master and other branch refs disallowed"
fi

# 5. Severity is declared explicitly so the gate is deterministic.
if grep -qE '^[[:space:]]+severity:[[:space:]]*[A-Za-z]+' "$WORKFLOW"; then
  ok "severity declared explicitly"
else
  fail "severity not declared — must set 'severity:' explicitly for deterministic gating"
fi

exit "$EXIT_CODE"
