#!/usr/bin/env bash
# Validate that pile-up-prone workflows declare a concurrency group (Issue #156).
#
# Workflows triggered on `pull_request` (and `push` for ci.yml) must declare a
# top-level `concurrency:` block so rapid successive pushes to a branch cancel
# superseded runs instead of stacking overlapping runs. This matters most for:
#   * ci.yml             — the heaviest workflow (full build/test/doc).
#   * auto-format.yml    — pushes a commit back to the PR branch (race risk).
#   * version-increment.yml — also pushes back to the PR branch (race risk).
#   * gitleaks.yml / semgrep.yml — wasted runner minutes on overlapping scans.
#
# The hardened pattern (mirrored from cargo-audit.yml, cargo-quality.yml,
# dependency-review.yml, markdown-lint.yml) is:
#
#   concurrency:
#     group: <workflow>-${{ github.ref }}
#     cancel-in-progress: true
#
# A workflow satisfies this validator when it declares:
#   1. A top-level `concurrency:` key.
#   2. A `group:` keyed by `${{ github.ref }}` (one in-flight run per ref).
#   3. `cancel-in-progress: true` (superseded runs are cancelled).
#
# The script takes a single optional `--workflow PATH` argument so BATS tests
# can exercise it against fixtures. When called with no argument it validates
# every pile-up-prone workflow under `.github/workflows/` relative to the repo
# root.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/check-harness.sh
source "$SCRIPT_DIR/lib/check-harness.sh"

usage() {
  cat <<'EOF'
Usage: check-workflow-concurrency.sh [--workflow PATH]

Options:
  --workflow PATH   Path to a single workflow YAML file to validate (default:
                    validate every pile-up-prone workflow under
                    .github/workflows/ relative to the repo root).
  -h, --help        Show this message.

Exits 0 when every checked workflow declares a concurrency group keyed by
${{ github.ref }} with cancel-in-progress: true. Exits non-zero with a
descriptive message otherwise.
EOF
}

parse_check_args --workflow "" "$@"
WORKFLOW="$CHECK_TARGET"

check_workflow() {
  local wf="$1"
  # Each ok/fail line is about this workflow file.
  local CHECK_SUBJECT="$wf"

  if [[ ! -f "$wf" ]]; then
    echo "Workflow file not found: $wf" >&2
    EXIT_CODE=2
    return
  fi

  # 1. Top-level concurrency: block.
  if ! grep -qE '^concurrency:[[:space:]]*$' "$wf"; then
    fail "no top-level 'concurrency:' block — overlapping runs will pile up"
    return
  fi
  ok "declares a top-level concurrency block"

  # 2. group: keyed by the ref so there is one in-flight run per branch/PR.
  if grep -qE '^[[:space:]]+group:[[:space:]]*.*\$\{\{[[:space:]]*github\.ref[[:space:]]*\}\}' "$wf"; then
    ok "concurrency group is keyed by github.ref"
  else
    fail "concurrency group is not keyed by \${{ github.ref }}"
  fi

  # 3. cancel-in-progress: true so superseded runs are cancelled.
  if grep -qE '^[[:space:]]+cancel-in-progress:[[:space:]]*true[[:space:]]*$' "$wf"; then
    ok "cancel-in-progress is true"
  else
    fail "cancel-in-progress is not set to true — superseded runs would keep running"
  fi
}

if [[ -n "$WORKFLOW" ]]; then
  check_workflow "$WORKFLOW"
else
  WF_DIR="$(check_repo_path ".github/workflows")"
  # Pile-up-prone workflows that must declare a concurrency group (Issue #156).
  PILE_UP_PRONE=(
    ci.yml
    auto-format.yml
    version-increment.yml
    gitleaks.yml
    semgrep.yml
  )
  for name in "${PILE_UP_PRONE[@]}"; do
    check_workflow "$WF_DIR/$name"
  done
fi

exit "$EXIT_CODE"
