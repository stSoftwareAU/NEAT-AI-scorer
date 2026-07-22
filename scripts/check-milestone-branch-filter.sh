#!/usr/bin/env bash
# Validate that the CI workflow's pull_request branch filter matches milestone
# feature branches (Issue #393).
#
# Milestone sub-issue PRs target a shared `milestone/<slug>` branch (planning
# delivery workflow). If the CI quality workflow's `pull_request.branches`
# filter does not match `milestone/*`, the gate never runs on those PRs and
# they merge into the milestone branch unchecked — the gap only surfaces later
# at the single rollup PR into the default branch. This validator fails loudly
# unless the filter includes a `milestone/*` glob so every intermediate
# sub-issue PR is gated too.
#
# This validator is purpose-built: it scans `ci.yml` with a minimal Python
# scanner rather than pulling in a YAML parser. Designed for reuse from BATS
# tests.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-milestone-branch-filter.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the CI workflow YAML file (default:
                    .github/workflows/ci.yml relative to repo root).
  -h, --help        Show this message.

Exits 0 when the workflow's on.pull_request.branches filter includes a
`milestone/*` glob. Exits non-zero with a descriptive message otherwise.
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
  WORKFLOW="$SCRIPT_DIR/../.github/workflows/ci.yml"
fi

if [[ ! -f "$WORKFLOW" ]]; then
  echo "Workflow file not found: $WORKFLOW" >&2
  exit 2
fi

# Extract the `on.pull_request.branches` list, one entry per line. The scanner
# walks the `on:` map, descends into `pull_request:`, then collects the
# `branches:` sequence in both inline (`[a, b]`) and block (`- a`) forms.
branches="$(
  python3 - "$WORKFLOW" <<'PY'
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as fh:
    lines = fh.read().splitlines()


def indent_of(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def strip_item(raw: str) -> str:
    raw = raw.strip()
    if (raw.startswith('"') and raw.endswith('"')) or (
        raw.startswith("'") and raw.endswith("'")
    ):
        raw = raw[1:-1]
    return raw


# Locate the top-level `on:` map (column 0). It may be written as `on:` or the
# YAML-normalised `true:` — accept only the literal `on:` here.
on_start = None
for i, line in enumerate(lines):
    if line.rstrip() == "on:":
        on_start = i + 1
        break

if on_start is None:
    sys.exit(0)

# Walk children of `on:` to find `pull_request:`.
pr_start = None
pr_indent = None
i = on_start
while i < len(lines):
    line = lines[i]
    if not line.strip() or line.lstrip().startswith("#"):
        i += 1
        continue
    ind = indent_of(line)
    if ind == 0:
        break  # left the `on:` map
    if pr_indent is None:
        pr_indent = ind
    if ind == pr_indent and line.strip().rstrip(":") == "pull_request":
        pr_start = i + 1
        break
    i += 1

if pr_start is None:
    sys.exit(0)

# Walk children of `pull_request:` to find `branches:`.
i = pr_start
child_indent = None
while i < len(lines):
    line = lines[i]
    if not line.strip() or line.lstrip().startswith("#"):
        i += 1
        continue
    ind = indent_of(line)
    if ind <= (pr_indent or 0):
        break  # left the pull_request block
    if child_indent is None:
        child_indent = ind
    if ind == child_indent and line.strip().startswith("branches:"):
        tail = line.strip()[len("branches:"):].strip()
        if tail:
            # inline form: branches: [a, b] or branches: a
            tail = tail.strip("[]")
            for item in tail.split(","):
                item = strip_item(item)
                if item:
                    print(item)
        else:
            # block sequence form
            m = i + 1
            while m < len(lines):
                nxt = lines[m]
                if not nxt.strip() or nxt.lstrip().startswith("#"):
                    m += 1
                    continue
                ni = indent_of(nxt)
                if ni <= child_indent:
                    break
                if nxt.lstrip().startswith("- "):
                    print(strip_item(nxt.strip()[2:]))
                m += 1
        break
    i += 1
PY
)"

if [[ -z "$branches" ]]; then
  echo "FAIL $WORKFLOW: no on.pull_request.branches filter found — cannot confirm milestone PRs are gated" >&2
  exit 1
fi

if grep -qx 'milestone/\*' <<<"$branches"; then
  echo "OK   $WORKFLOW: pull_request.branches includes 'milestone/*' — milestone PRs are gated"
  exit 0
fi

echo "FAIL $WORKFLOW: pull_request.branches must include 'milestone/*' so milestone sub-issue PRs are gated (found: $(tr '\n' ' ' <<<"$branches"))" >&2
exit 1
