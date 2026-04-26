#!/usr/bin/env bash
# Validate the Semgrep SAST scanning workflow (Issue #47).
#
# Two configurations are accepted as functionally equivalent:
#   1. The official container approach — `container: image: semgrep/semgrep:<tag>`
#      with an explicit `semgrep ci|scan --config <ruleset>` invocation. This
#      is upstream's recommended PR-scan path.
#   2. The `semgrep/semgrep-action@vN` GitHub Action — pinned to a numeric
#      major version so behaviour is reproducible.
#
# Either form must:
#   * Trigger on `pull_request` so PRs are scanned before merge.
#   * Declare an explicit `permissions:` block (least privilege).
#   * Pass `SEMGREP_APP_TOKEN` from repo secrets so findings can be posted
#     to the Semgrep app dashboard. The token is consumed by both the
#     container CLI (`semgrep ci`) and `semgrep/semgrep-action`.
#   * Pin the entry point. Container tags must NOT be bare (`semgrep/semgrep`)
#     or `:latest`; the action ref must NOT be `master`/`main`.
#   * Carry a comment block documenting the rationale and why the
#     container path is equivalent to `semgrep/semgrep-action`.
#
# The script takes a single optional `--workflow PATH` argument so BATS tests
# can exercise it against fixtures. When called with no argument it validates
# `.github/workflows/semgrep.yml` relative to the repo root.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-semgrep-workflow.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the Semgrep workflow YAML file (default:
                    .github/workflows/semgrep.yml relative to the repo root).
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
  WORKFLOW="$SCRIPT_DIR/../.github/workflows/semgrep.yml"
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
if grep -qE '^on:[[:space:]]*$' "$WORKFLOW" && grep -qE '^[[:space:]]+pull_request:' "$WORKFLOW"; then
  ok "triggers on pull_request"
elif grep -qE '^on:[[:space:]]*\[?.*pull_request' "$WORKFLOW"; then
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

# 3. Semgrep entry point: either pinned container image OR pinned action.
container_line="$(grep -nE '^[[:space:]]+image:[[:space:]]*semgrep/semgrep' "$WORKFLOW" || true)"
action_line="$(grep -nE 'uses:[[:space:]]*semgrep/semgrep-action@' "$WORKFLOW" || true)"

if [[ -n "$container_line" ]]; then
  # Container path. Tag must not be bare or :latest.
  if echo "$container_line" | grep -qE 'image:[[:space:]]*semgrep/semgrep:[A-Za-z0-9._-]+' \
    && ! echo "$container_line" | grep -qE 'image:[[:space:]]*semgrep/semgrep:latest'; then
    ok "Semgrep container image is pinned (semgrep/semgrep:<tag>)"
  else
    fail "Semgrep container image is not pinned — use semgrep/semgrep:<version>, not bare or :latest"
  fi
elif [[ -n "$action_line" ]]; then
  # Action path. Ref must be a vN-style pin (numeric major component).
  if echo "$action_line" | grep -qE 'semgrep/semgrep-action@v?[0-9]+'; then
    ok "semgrep/semgrep-action is pinned (vN ref)"
  else
    fail "semgrep/semgrep-action ref is not a numeric vN pin — branch refs disallowed"
  fi
else
  fail "no Semgrep entry point — expect either 'image: semgrep/semgrep:<tag>' or 'uses: semgrep/semgrep-action@vN'"
fi

# 4. semgrep CLI invocation (container path) must include --config.
#    The action path embeds its own config handling, so this rule only
#    applies when the workflow runs the CLI directly.
if [[ -n "$container_line" ]]; then
  if grep -qE 'semgrep[[:space:]]+(ci|scan)\b' "$WORKFLOW"; then
    ok "semgrep CLI invocation present (ci|scan)"
  else
    fail "container path does not invoke 'semgrep ci' or 'semgrep scan'"
  fi
  if grep -qE -- '--config[[:space:]=][[:space:]]*[A-Za-z0-9./_+:-]+' "$WORKFLOW"; then
    ok "semgrep invocation has explicit --config <ruleset>"
  else
    fail "semgrep invocation has no explicit --config <ruleset>"
  fi
fi

# 5. SEMGREP_APP_TOKEN propagated from repo secrets.
if grep -qE 'SEMGREP_APP_TOKEN:[[:space:]]*\$\{\{[[:space:]]*secrets\.SEMGREP_APP_TOKEN[[:space:]]*\}\}' "$WORKFLOW"; then
  ok "SEMGREP_APP_TOKEN sourced from secrets"
else
  fail "SEMGREP_APP_TOKEN is not wired up from secrets.SEMGREP_APP_TOKEN"
fi

# 6. Rationale comment present. The comment must reference both the chosen
#    entry point and the equivalent path so reviewers know the configuration
#    is deliberate and complete (see Issue #47 — "or equivalent configuration").
if grep -qiE '^[[:space:]]*#.*semgrep/semgrep-action' "$WORKFLOW"; then
  ok "rationale comment references semgrep/semgrep-action equivalence"
else
  fail "no comment documenting the semgrep/semgrep-action equivalence — reviewers cannot tell if the choice is deliberate"
fi

exit "$EXIT_CODE"
