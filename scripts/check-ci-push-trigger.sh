#!/usr/bin/env bash
# Validate that the CI checker workflow does not re-trigger on push to the
# default branch `Develop` (Issue #370).
#
# `ci.yml` runs the heavy test/lint/scan gate. As a *checker* it should gate the
# pull request, not re-run post-merge: a push-to-`Develop` trigger duplicates
# the run that already gated the PR — wasting CI minutes and risking a red tick
# on the default branch for a check that already passed. Deploy/publish/release
# workflows are different (they must keep firing on push); this is a checker.
#
# The `pull_request` filter legitimately lists `Develop` (PRs into the default
# branch must be gated), so this validator scopes strictly to the `on.push`
# trigger: it fails only when the *push* branch filter lists `Develop`. A
# workflow with no `push` trigger at all passes — a checker that gates only the
# PR is exactly the desired end state.
#
# This validator is purpose-built: it scans the workflow with a minimal Python
# scanner rather than pulling in a YAML parser. Designed for reuse from BATS
# tests and `quality.sh`.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-ci-push-trigger.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the CI workflow YAML file (default:
                    .github/workflows/ci.yml relative to repo root).
  -h, --help        Show this message.

Exits 0 when the workflow's on.push trigger does not target the default branch
`Develop` (or has no push trigger). Exits non-zero with a descriptive message
when the push branch filter lists `Develop`.
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

# Detect whether a `push:` trigger exists at all, and extract its `branches`
# list (one entry per line) if present. The scanner walks the `on:` map,
# descends into `push:`, then collects the `branches:` sequence in both inline
# (`[a, b]`) and block (`- a`) forms. A leading "PUSH_PRESENT" marker line
# distinguishes "no push trigger" from "push trigger with an empty filter".
scan="$(
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


# Locate the top-level `on:` map (column 0). Accept only the literal `on:`.
on_start = None
for i, line in enumerate(lines):
    if line.rstrip() == "on:":
        on_start = i + 1
        break

if on_start is None:
    sys.exit(0)

# Walk children of `on:` to find `push:`.
push_start = None
push_indent = None
child_indent = None
i = on_start
while i < len(lines):
    line = lines[i]
    if not line.strip() or line.lstrip().startswith("#"):
        i += 1
        continue
    ind = indent_of(line)
    if ind == 0:
        break  # left the `on:` map
    if child_indent is None:
        child_indent = ind
    if ind == child_indent and line.strip().rstrip(":") == "push":
        push_start = i + 1
        push_indent = ind
        break
    i += 1

if push_start is None:
    sys.exit(0)

print("PUSH_PRESENT")

# Walk children of `push:` to find `branches:`.
i = push_start
push_child_indent = None
while i < len(lines):
    line = lines[i]
    if not line.strip() or line.lstrip().startswith("#"):
        i += 1
        continue
    ind = indent_of(line)
    if ind <= push_indent:
        break  # left the push block
    if push_child_indent is None:
        push_child_indent = ind
    if ind == push_child_indent and line.strip().startswith("branches:"):
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
                if ni <= push_child_indent:
                    break
                if nxt.lstrip().startswith("- "):
                    print(strip_item(nxt.strip()[2:]))
                m += 1
        break
    i += 1
PY
)"

# No `push:` trigger at all — the checker gates only the PR, which is the goal.
if ! grep -qx 'PUSH_PRESENT' <<<"$scan"; then
  echo "OK   $WORKFLOW: no push trigger — the checker gates the PR, not push to Develop"
  exit 0
fi

branches="$(grep -vx 'PUSH_PRESENT' <<<"$scan" || true)"

if grep -qxE 'Develop' <<<"$branches"; then
  echo "FAIL $WORKFLOW: on.push.branches lists the default branch Develop — a checker must not re-run on push to Develop (Issue #370)" >&2
  exit 1
fi

echo "OK   $WORKFLOW: on.push.branches does not target the default branch Develop"
exit 0
