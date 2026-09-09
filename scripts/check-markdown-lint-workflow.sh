#!/usr/bin/env bash
# Validate the standalone Markdown Lint workflow (Issue #63).
#
# The markdown-lint workflow must:
#   1. Trigger on pull_request so every change is linted before merge.
#   2. Declare an explicit `permissions:` block (least privilege —
#      `contents: read` is sufficient because markdownlint only reads source).
#   3. Pin `actions/checkout` to a numeric major version or a 40-char SHA
#      (Node 24 policy — see scripts/check-workflow-action-versions.sh).
#   4. Provision Node via `actions/setup-node` pinned to a numeric major.
#   5. Install and invoke `markdownlint-cli2` so the lint gate actually runs.
#   6. Pin that install to an exact `@x.y.z` version — a `run:` block is not
#      covered by `uses:` SHA-pinning or by the dependency quarantine, so an
#      unpinned install executes whatever the registry serves (Issue #594).
#
# The script takes a single optional `--workflow PATH` argument so BATS tests
# can exercise it against fixtures. When called with no argument it validates
# `.github/workflows/markdown-lint.yml` relative to the repo root.
set -euo pipefail

# Shared workflow-validation helpers (Issues #511, #514) — the `actions/checkout`
# pin rule and the least-privilege `permissions:` rule each live in one place
# instead of inline copies that drift apart.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKFLOW_CHECKS_LIB="$SCRIPT_DIR/lib/workflow-checks.sh"
if [[ ! -r "$WORKFLOW_CHECKS_LIB" ]]; then
  echo "Missing shared helper: $WORKFLOW_CHECKS_LIB" >&2
  exit 2
fi
# shellcheck source=lib/workflow-checks.sh disable=SC1091
source "$WORKFLOW_CHECKS_LIB"

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

# 2. Explicit permissions block (least privilege), via the shared rule in
#    scripts/lib/workflow-checks.sh (Issue #514).
require_readonly_permissions "$WORKFLOW"

# 3. actions/checkout pinned to a numeric major (vN) or a 40-char SHA, via the
#    shared rule in scripts/lib/workflow-checks.sh (Issue #511). Branch refs
#    disallowed.
require_pinned_checkout "$WORKFLOW"

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
# Comment lines are skipped: the workflow documents its own pinning policy in
# prose above the step, and a commented mention is not an install step.
install_line="$(grep -nE 'npm[[:space:]]+install[[:space:]].*markdownlint-cli2' "$WORKFLOW" \
  | grep -vE '^[0-9]+:[[:space:]]*#' | head -n 1 || true)"
if [[ -n "$install_line" ]]; then
  ok "markdownlint-cli2 install step present"
else
  fail "markdownlint-cli2 install step missing — gate cannot run without the binary"
fi

# 6. That install must pin an EXACT version (Issue #594). `uses:` SHA-pinning
#    (rules 3 and 4) does not reach inside a `run:` block, and the repository's
#    dependency quarantine only covers manifests a bump tool can manage — a
#    `run:` block is neither. An unpinned `npm install -g <pkg>` therefore
#    resolves whatever the registry serves at that moment, so a hijacked or
#    malicious release executes on the runner, under the workflow's
#    GITHUB_TOKEN, the instant it is published and with no embargo.
#
#    Accepted:  markdownlint-cli2@0.23.2, markdownlint-cli2@1.0.0-beta.1
#    Rejected:  bare name, dist-tags (@latest, @next), and every range form
#               (@^0.23.2, @~0.23, @>=0.23.0, @0.x, @*) — all of them re-resolve
#               on a later run, which is the exact hazard being closed.
if [[ -n "$install_line" ]]; then
  version_spec="$(grep -oE 'markdownlint-cli2@[^[:space:]"'"'"'`]+' <<<"$install_line" | head -n 1 || true)"
  version="${version_spec#markdownlint-cli2@}"
  if [[ -z "$version" ]]; then
    fail "markdownlint-cli2 install is not pinned to an exact version — no '@<version>' on the install; a floating install runs whatever the registry serves at that moment (Issue #594)"
  elif [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
    ok "markdownlint-cli2 install pinned to exact version $version"
  else
    fail "markdownlint-cli2 install is not pinned to an exact version — '@$version' is a dist-tag or range that re-resolves on every run; use an exact x.y.z (Issue #594)"
  fi
fi

if grep -qE '^[[:space:]]+(- )?run:[[:space:]]*markdownlint-cli2' "$WORKFLOW"; then
  ok "markdownlint-cli2 invoked"
else
  fail "markdownlint-cli2 is not invoked — no 'run: markdownlint-cli2' step found"
fi

# 7. A lint/checker workflow must gate the PR only — it must NOT trigger on
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
