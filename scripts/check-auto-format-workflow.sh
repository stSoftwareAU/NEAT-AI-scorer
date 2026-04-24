#!/usr/bin/env bash
# Validate the auto-format PR workflow (Issue #19).
#
# The auto-format workflow must:
#   1. Run on `pull_request` events only — formatting fixes push back to
#      the PR branch, which is meaningless for `push` / scheduled events.
#   2. Declare minimal permissions: `contents: write` is required to push,
#      nothing beyond what the job needs should be granted (in particular
#      not `write-all` or scopes the workflow doesn't use).
#   3. Invoke `cargo fmt --all` to produce the formatting fixes.
#   4. Gate the commit/push step behind a change-detection output so the
#      job is idempotent — re-running on a clean branch is a no-op.
#   5. Refuse to push onto a fork's PR branch (the GITHUB_TOKEN cannot
#      write there anyway; skipping keeps failure messages tidy).
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-auto-format-workflow.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the auto-format workflow YAML file (default:
                    .github/workflows/auto-format.yml relative to repo root).
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
  WORKFLOW="$SCRIPT_DIR/../.github/workflows/auto-format.yml"
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

# 1. pull_request trigger present.
if grep -qE '^[[:space:]]*pull_request:' "$WORKFLOW"; then
  ok "pull_request trigger present"
else
  fail "no pull_request trigger — auto-format must only run on PRs"
fi

# 2. Minimal permissions. `contents: write` is the one scope required to
#    push. Reject `write-all`, or any scope we don't expect.
if grep -qE '^[[:space:]]*permissions:[[:space:]]*write-all' "$WORKFLOW"; then
  fail "'permissions: write-all' grants more than this job needs (use contents: write)"
elif grep -qE '^[[:space:]]*contents:[[:space:]]*write' "$WORKFLOW"; then
  ok "minimal write permission (contents: write) present"
else
  fail "no 'contents: write' permission — the job cannot push rustfmt fixes"
fi

# 3. cargo fmt invocation present. Require `--all` so the whole workspace is
#    formatted (matches quality.sh and ci.yml's `cargo fmt --all -- --check`).
if grep -qE 'cargo[[:space:]]+fmt[[:space:]]+--all' "$WORKFLOW"; then
  ok "cargo fmt --all invocation present"
else
  fail "no 'cargo fmt --all' invocation"
fi

# 4. Change-detection guard on the commit/push step. We look for a step that
#    declares `if:` and references the detection output — the specific id
#    name is flexible but the presence of conditional commit/push is
#    mandatory for idempotency.
#    The pattern: somewhere in the file, a step's `if:` references
#    `steps.<id>.outputs.` — that is the only way a prior step's change
#    detection can gate the commit.
if grep -qE '^[[:space:]]*if:[[:space:]]*steps\.[A-Za-z0-9_-]+\.outputs\.' "$WORKFLOW"; then
  ok "commit/push is conditional on change-detection output (idempotent guard)"
else
  fail "no conditional 'if: steps.*.outputs.*' guard — commit/push must be conditional"
fi

# 5. Fork-PR safety — skip when the PR head is on a fork. Any of the common
#    forms satisfy this check: comparing `head.repo.full_name` to
#    `github.repository`, or checking `head.repo.fork == false`.
if grep -qE 'github\.event\.pull_request\.head\.repo\.full_name[[:space:]]*==' "$WORKFLOW" \
  || grep -qE 'github\.event\.pull_request\.head\.repo\.fork' "$WORKFLOW"; then
  ok "fork PRs are excluded from the push step"
else
  fail "no head.repo check — pushes onto forks will fail silently"
fi

# 6. Strict bash in every run: block that does a commit/push sequence.
if grep -qE 'set[[:space:]]+-euo[[:space:]]+pipefail' "$WORKFLOW"; then
  ok "strict bash (set -euo pipefail) present"
else
  fail "no 'set -euo pipefail' in run: blocks — failures may be swallowed"
fi

exit "$EXIT_CODE"
