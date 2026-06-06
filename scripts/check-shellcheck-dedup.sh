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
# `.github/workflows` may invoke `ludeeus/action-shellcheck`. Re-introducing a
# second invocation re-creates the duplicated maintenance surface and fails.
#
# The script takes an optional `--workflows DIR` argument so BATS tests can
# exercise it against fixtures. With no argument it scans
# `.github/workflows` relative to the repo root.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-shellcheck-dedup.sh [--workflows DIR]

Options:
  --workflows DIR   Directory containing workflow YAML files (default:
                    .github/workflows relative to the repo root).
  -h, --help        Show this message.

Exits 0 when exactly one workflow invokes ludeeus/action-shellcheck. Exits
non-zero with a descriptive message when ShellCheck is missing entirely or
duplicated across more than one workflow file.
EOF
}

WORKFLOWS_DIR=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --workflows)
      WORKFLOWS_DIR="$2"
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

if [[ -z "$WORKFLOWS_DIR" ]]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  WORKFLOWS_DIR="$SCRIPT_DIR/../.github/workflows"
fi

if [[ ! -d "$WORKFLOWS_DIR" ]]; then
  echo "Workflows directory not found: $WORKFLOWS_DIR" >&2
  exit 2
fi

# Collect every workflow file that *invokes* ShellCheck. We match `uses:` lines
# only (ignoring leading `- `) and skip comment lines so that prose mentioning
# the action does not count as an invocation.
matches=()
while IFS= read -r workflow; do
  [[ -f "$workflow" ]] || continue
  if grep -qE '^[[:space:]]*(-[[:space:]]*)?uses:[[:space:]]*ludeeus/action-shellcheck@' "$workflow"; then
    matches+=("$workflow")
  fi
done < <(find "$WORKFLOWS_DIR" -maxdepth 1 \( -name "*.yml" -o -name "*.yaml" \) -type f | sort)

count="${#matches[@]}"

if [[ "$count" -eq 1 ]]; then
  echo "OK   ShellCheck invoked in exactly one workflow: ${matches[0]}"
  exit 0
fi

if [[ "$count" -eq 0 ]]; then
  echo "FAIL no workflow invokes ludeeus/action-shellcheck — ShellCheck coverage is missing." >&2
  exit 1
fi

echo "FAIL ShellCheck duplicated across $count workflows — keep exactly one home (Issue #157):" >&2
printf '  - %s\n' "${matches[@]}" >&2
exit 1
