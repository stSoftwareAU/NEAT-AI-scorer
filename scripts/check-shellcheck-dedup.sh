#!/usr/bin/env bash
# Guard against duplicating the ShellCheck logical check (Issue #157).
#
# ShellCheck must live in exactly ONE workflow file. Historically the same
# `ludeeus/action-shellcheck` invocation ran in both `ci.yml`'s `shell-checks`
# job and a standalone `shellcheck.yml`, so a config change (severity, ignore
# paths, a SHA bump) had to be made in lockstep in both files or the two scans
# silently diverged. Issue #157 collapsed ShellCheck back to a single home —
# `ci.yml`'s `shell-checks` job, which already feeds the `ci-required`
# aggregator that branch protection gates on.
#
# This guard asserts that invariant: exactly one workflow under
# `.github/workflows` may invoke ShellCheck. ShellCheck can be invoked either
# via the `ludeeus/action-shellcheck` action or by calling the pre-installed
# `shellcheck` binary directly in a `run:` step (PR #184 switched `ci.yml` to
# the latter to drop the flaky release-asset download). Re-introducing a second
# invocation by either method re-creates the duplicated maintenance surface and
# fails.
#
# The script takes an optional `--workflows DIR` argument so BATS tests can
# exercise it against fixtures. With no argument it scans
# `.github/workflows` relative to the repo root.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/check-harness.sh
source "$SCRIPT_DIR/lib/check-harness.sh"

usage() {
  cat <<'EOF'
Usage: check-shellcheck-dedup.sh [--workflows DIR]

Options:
  --workflows DIR   Directory containing workflow YAML files (default:
                    .github/workflows relative to the repo root).
  -h, --help        Show this message.

Exits 0 when exactly one workflow invokes ShellCheck (via
ludeeus/action-shellcheck or a direct `shellcheck --severity` run step).
Exits non-zero with a descriptive message when ShellCheck is missing entirely
or duplicated across more than one workflow file.
EOF
}

parse_check_args --workflows ".github/workflows" "$@"
WORKFLOWS_DIR="$CHECK_TARGET"
check_require_dir "$WORKFLOWS_DIR"

# Collect every workflow file that *invokes* ShellCheck. A workflow counts when
# it either references the action on a `uses:` line (ignoring leading `- `) or
# runs the `shellcheck --severity` binary in a `run:` step. Both patterns anchor
# to the start of the line so prose mentioning ShellCheck in a comment does not
# count as an invocation.
matches=()
while IFS= read -r workflow; do
  [[ -f "$workflow" ]] || continue
  if grep -qE '^[[:space:]]*(-[[:space:]]*)?uses:[[:space:]]*ludeeus/action-shellcheck@' "$workflow" \
    || grep -qE '^[[:space:]]*shellcheck[[:space:]]+--severity' "$workflow"; then
    matches+=("$workflow")
  fi
done < <(find "$WORKFLOWS_DIR" -maxdepth 1 \( -name "*.yml" -o -name "*.yaml" \) -type f | sort)

count="${#matches[@]}"

if [[ "$count" -eq 1 ]]; then
  echo "OK   ShellCheck invoked in exactly one workflow: ${matches[0]}"
  exit 0
fi

if [[ "$count" -eq 0 ]]; then
  echo "FAIL no workflow invokes ShellCheck — ShellCheck coverage is missing." >&2
  exit 1
fi

echo "FAIL ShellCheck duplicated across $count workflows — keep exactly one home (Issue #157):" >&2
printf '  - %s\n' "${matches[@]}" >&2
exit 1
