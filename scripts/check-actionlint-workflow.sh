#!/usr/bin/env bash
# Validate the standalone Actionlint workflow (Issue #195).
#
# The actionlint workflow must:
#   1. Trigger on pull_request so every change is linted before merge.
#   2. Declare an explicit `permissions:` block (least privilege —
#      `contents: read` is sufficient because actionlint only reads source).
#   3. Pin `actions/checkout` to a numeric major version (Node 24 policy —
#      see scripts/check-workflow-action-versions.sh).
#   4. Install the actionlint binary at a pinned version (the official
#      download-actionlint.bash installer fetched from a version-pinned
#      upstream ref — no third-party wrapper action).
#   5. Invoke `actionlint` so the lint gate actually runs.
#   6. Cover milestone/<slug> branches in the pull_request filter so the gate
#      runs on milestone sub-issue PRs, not only top-level base branches
#      (Issue #390). A bare "*" glob does not match refs containing a slash.
#   7. Not re-trigger on push to the default branch `Develop` (Issue #369). A
#      lint gate belongs on the PR; a post-merge push run duplicates the check
#      that already gated the PR, wasting CI minutes and risking a stray red
#      tick on the default branch.
#
# The script takes a single optional `--workflow PATH` argument so BATS tests
# can exercise it against fixtures. When called with no argument it validates
# `.github/workflows/actionlint.yml` relative to the repo root.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-actionlint-workflow.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the actionlint workflow YAML file (default:
                    .github/workflows/actionlint.yml relative to the repo
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
  WORKFLOW="$SCRIPT_DIR/../.github/workflows/actionlint.yml"
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

# 3. actions/checkout pinned to a numeric major (vN) or SHA. Branch refs
#    disallowed (SHA pins carry a trailing `# vN` label).
checkout_line="$(grep -nE 'uses:[[:space:]]*actions/checkout@' "$WORKFLOW" || true)"
if [[ -z "$checkout_line" ]]; then
  fail "actions/checkout step missing — workflow cannot fetch the repo"
elif echo "$checkout_line" | grep -qE 'actions/checkout@([0-9a-f]{40}|v?[0-9]+)'; then
  ok "actions/checkout pinned to a numeric major or SHA"
else
  fail "actions/checkout is not pinned — branch refs disallowed"
fi

# 4. actionlint must be installed. The official installer is
#    download-actionlint.bash; require a reference to it so the binary is
#    actually provisioned before the gate runs.
if grep -qE 'download-actionlint\.bash' "$WORKFLOW"; then
  ok "actionlint install step present"
else
  fail "actionlint install step missing — gate cannot run without the binary"
fi

# 5. actionlint must be invoked in a run step.
if grep -qE '^[[:space:]]+(- )?run:[[:space:]]*actionlint' "$WORKFLOW" \
  || grep -qE '^[[:space:]]+actionlint( |$)' "$WORKFLOW"; then
  ok "actionlint invoked"
else
  fail "actionlint is not invoked — no 'run: actionlint' step found"
fi

# 6. pull_request branch filter must include milestone branches so the gate
#    runs on milestone/<slug> sub-issue PRs. A bare "*" glob only matches
#    single-level refs, so milestone/<slug> would otherwise skip the gate and
#    merge unchecked into the milestone branch (Issue #390). The token
#    `milestone/*` also matches the wider `milestone/**` glob.
if grep -qE 'milestone/\*' "$WORKFLOW"; then
  ok "pull_request branch filter covers milestone/* branches"
else
  fail "pull_request branch filter omits milestone/* — milestone PRs skip the gate"
fi

# 7. The lint gate must not re-trigger on push to the default branch `Develop`
#    (Issue #369). actionlint is a checker: it should gate the PR, not re-run
#    post-merge. A push-to-`Develop` trigger duplicates the run that already
#    gated the PR — wasting CI minutes and risking a red tick on the default
#    branch for a check that already passed. Deploy/release workflows differ
#    (they must keep firing on push); this is a lint gate. Any `branches:`
#    filter listing `Develop` (only the push filter ever does) fails this rule.
if grep -E '^[[:space:]]+branches:' "$WORKFLOW" \
  | grep -qE '(^|[^[:alnum:]_])Develop([^[:alnum:]_]|$)'; then
  fail "branch filter lists the default branch Develop — a lint gate must not re-run on push to Develop (Issue #369)"
else
  ok "does not re-trigger on push to the default branch Develop"
fi

exit "$EXIT_CODE"
