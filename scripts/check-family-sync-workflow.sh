#!/usr/bin/env bash
# Validate the family-sync workflow (Issue #629).
#
# `scripts/runlib.sh` is a byte-identical copy of NEAT-AI-core Develop's copy,
# kept fresh by a CI job rather than by hand. The workflow must:
#   1. Run on `pull_request` events only — the refresh pushes back to the PR
#      branch, which is meaningless for `push` / scheduled events.
#   2. Gate milestone PRs too: a `milestone/**` branch filter, so a sub-issue
#      PR is not silently unsynced.
#   3. Declare a `family-sync` job — the name the fleet's workflow audit and
#      this repository's docs both refer to.
#   4. Declare minimal permissions: `contents: write` is what the push needs
#      and nothing more (in particular not `write-all`).
#   5. Invoke `scripts/family-sync.sh`, which owns the fetch/compare/write
#      contract and fails non-zero on a fetch error.
#   6. Gate the commit/push step behind a change-detection output so a branch
#      that is already in sync is a no-op.
#   7. Refuse to push onto a fork's PR branch.
#   8. Rebase onto the remote head before pushing, so a commit pushed while
#      the job ran is never clobbered.
#   9. Authenticate the push with ACTIONS_PUSH (GITHUB_TOKEN fallback), the
#      Issue #435 pattern shared with version-increment.yml.
#  10. Use strict bash in its run: blocks.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/check-harness.sh
source "$SCRIPT_DIR/lib/check-harness.sh"

usage() {
  cat <<'EOF'
Usage: check-family-sync-workflow.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the family-sync workflow YAML file (default:
                    .github/workflows/family-sync.yml relative to repo root).
  -h, --help        Show this message.

Exits 0 when the workflow satisfies every rule listed in the script header.
Exits non-zero with a descriptive message otherwise.
EOF
}

parse_check_args --workflow ".github/workflows/family-sync.yml" "$@"
WORKFLOW="$CHECK_TARGET"
check_require_file "$WORKFLOW"
check_subject "$WORKFLOW"

# 1. pull_request trigger, and no push trigger (the sync commits back to a PR).
if grep -qE '^[[:space:]]*pull_request:' "$WORKFLOW"; then
  ok "pull_request trigger present"
else
  fail "no pull_request trigger — the sync must run on PRs"
fi
if grep -qE '^[[:space:]]{2}push:' "$WORKFLOW"; then
  fail "push trigger present — a push-triggered sync would commit onto Develop"
else
  ok "no push trigger"
fi

# 2. Milestone PRs are gated too. GitHub's `*` glob does not cross `/`, so a
#    bare `"*"` filter would skip every `milestone/<slug>` branch.
if grep -qE '^[[:space:]]*-[[:space:]]*"?milestone/\*' "$WORKFLOW" \
  || grep -qE '^[[:space:]]*-[[:space:]]*"?\*\*"?[[:space:]]*$' "$WORKFLOW"; then
  ok "milestone/** PRs are covered by the branch filter"
else
  fail "branch filter misses milestone/** PRs — sub-issue branches would go unsynced"
fi

# 3. The job is named family-sync.
if grep -qE '^[[:space:]]{2}family-sync:[[:space:]]*$' "$WORKFLOW"; then
  ok "declares the family-sync job"
else
  fail "no 'family-sync:' job — the audited job name must not drift"
fi

# 4. Minimal permissions.
if grep -qE '^[[:space:]]*permissions:[[:space:]]*write-all' "$WORKFLOW"; then
  fail "'permissions: write-all' grants more than this job needs (use contents: write)"
elif grep -qE '^[[:space:]]*contents:[[:space:]]*write' "$WORKFLOW"; then
  ok "minimal write permission (contents: write) present"
else
  fail "no 'contents: write' permission — the job cannot push the refreshed copy"
fi

# 5. The fetch/compare/write contract lives in the script, not in the YAML.
if grep -qE 'scripts/family-sync\.sh' "$WORKFLOW"; then
  ok "invokes scripts/family-sync.sh"
else
  fail "no 'scripts/family-sync.sh' invocation — the sync contract must not be re-spelt in YAML"
fi

# 6. Change-detection guard on the commit/push step.
if grep -qE '^[[:space:]]*if:[[:space:]]*steps\.[A-Za-z0-9_-]+\.outputs\.' "$WORKFLOW"; then
  ok "commit/push is conditional on change-detection output (idempotent guard)"
else
  fail "no conditional 'if: steps.*.outputs.*' guard — commit/push must be conditional"
fi

# 7. Fork-PR safety.
if grep -qE 'github\.event\.pull_request\.head\.repo\.full_name[[:space:]]*==' "$WORKFLOW" \
  || grep -qE 'github\.event\.pull_request\.head\.repo\.fork' "$WORKFLOW"; then
  ok "fork PRs are excluded from the push step"
else
  fail "no head.repo check — pushes onto forks will fail silently"
fi

# 8. Rebase before push.
if grep -qE 'pull[[:space:]]+--rebase|rebase[[:space:]]+' "$WORKFLOW"; then
  ok "rebases onto the remote head before pushing"
else
  fail "no rebase before push — a commit pushed during the run would be clobbered"
fi

# 9. Bot push identity (Issue #435).
if grep -qE 'secrets\.ACTIONS_PUSH[[:space:]]*\|\|[[:space:]]*secrets\.GITHUB_TOKEN' "$WORKFLOW"; then
  ok "push authenticates with ACTIONS_PUSH (GITHUB_TOKEN fallback)"
else
  fail "no 'secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN' — bot pushes will gate PR checks behind Approve and run (Issue #435)"
fi

# 10. Strict bash.
if grep -qE 'set[[:space:]]+-euo[[:space:]]+pipefail' "$WORKFLOW"; then
  ok "strict bash (set -euo pipefail) present"
else
  fail "no 'set -euo pipefail' in run: blocks — failures may be swallowed"
fi

exit "$EXIT_CODE"
