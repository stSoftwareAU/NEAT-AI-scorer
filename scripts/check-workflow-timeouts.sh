#!/usr/bin/env bash
# Validate that every job across .github/workflows/ declares an explicit
# `timeout-minutes` (Issue #154).
#
# Background: a job without `timeout-minutes` inherits GitHub's 360-minute
# (6-hour) default. A hung compile, a wedged `cargo install`, a stuck network
# fetch, or a runaway test can then occupy a shared runner for the full six
# hours before it is reclaimed — delaying queued runs, burning runner minutes,
# and masking a genuinely stuck step behind a long, silent wait. An explicit
# per-job timeout converts a hang into a fast, legible failure.
#
# Rules enforced, per job in each workflow:
#   1. A normal job (one that runs steps on a runner) MUST declare a
#      job-level `timeout-minutes:`.
#   2. The declared value MUST be a positive integer no greater than 360
#      (GitHub's own ceiling — a larger value is silently capped and signals a
#      mistake).
#   3. A reusable-workflow-call job (a job whose body is a `uses:` reference)
#      MUST NOT declare `timeout-minutes` — GitHub rejects that keyword on
#      caller jobs; the timeout belongs in the called workflow's own jobs.
#
# The script accepts zero or more `--workflow PATH` arguments so BATS tests can
# exercise it against fixtures. With no argument it validates every
# `*.yml`/`*.yaml` file under `.github/workflows/` relative to the repo root.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-workflow-timeouts.sh [--workflow PATH]...

Options:
  --workflow PATH   Path to a workflow YAML file to validate. May be repeated.
                    With no --workflow argument, every *.yml/*.yaml file under
                    .github/workflows/ (relative to the repo root) is checked.
  -h, --help        Show this message.

Exits 0 when every normal job declares a sane `timeout-minutes` and no
reusable-call job declares one. Exits non-zero with a descriptive message
otherwise.
EOF
}

WORKFLOWS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --workflow)
      [[ $# -ge 2 ]] || { echo "--workflow requires a PATH argument" >&2; exit 2; }
      WORKFLOWS+=("$2")
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

if [[ ${#WORKFLOWS[@]} -eq 0 ]]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  WF_DIR="$SCRIPT_DIR/../.github/workflows"
  while IFS= read -r f; do
    WORKFLOWS+=("$f")
  done < <(find "$WF_DIR" -maxdepth 1 -type f \( -name '*.yml' -o -name '*.yaml' \) | sort)
fi

if [[ ${#WORKFLOWS[@]} -eq 0 ]]; then
  echo "No workflow files to validate" >&2
  exit 2
fi

EXIT_CODE=0
fail() {
  echo "FAIL $1: ${*:2}" >&2
  EXIT_CODE=1
}
ok() {
  echo "OK   $1: ${*:2}"
}

# Maximum permitted timeout — GitHub caps a job at 360 minutes regardless.
MAX_TIMEOUT=360

validate_workflow() {
  local wf="$1"

  if [[ ! -f "$wf" ]]; then
    echo "Workflow file not found: $wf" >&2
    EXIT_CODE=1
    return
  fi

  # Emit a TSV report, one line per job:
  #   JOB\t<name>\t<is_reusable 0|1>\t<has_timeout 0|1>\t<timeout_value|->
  # A minimal indentation-based scanner walks the YAML rather than pulling in a
  # YAML parser — the same approach as check-ci-permissions.sh.
  local report
  report="$(
    python3 - "$wf" <<'PY'
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as fh:
    lines = fh.read().splitlines()

def indent_of(line):
    return len(line) - len(line.lstrip(" "))

def is_blank_or_comment(line):
    s = line.strip()
    return (not s) or s.startswith("#")

# Locate the top-level `jobs:` map.
jobs_start = None
for i, line in enumerate(lines):
    if indent_of(line) == 0 and line.strip() == "jobs:":
        jobs_start = i + 1
        break

if jobs_start is None:
    sys.exit(0)

# Determine the job-key indent (first non-blank line under `jobs:`).
job_indent = None
for i in range(jobs_start, len(lines)):
    if is_blank_or_comment(lines[i]):
        continue
    ind = indent_of(lines[i])
    if ind == 0:
        break
    job_indent = ind
    break

if job_indent is None:
    sys.exit(0)

child_indent = job_indent + 2

# Collect job boundaries.
jobs = []  # (name, start_line, end_line)
i = jobs_start
current = None
while i < len(lines):
    line = lines[i]
    if is_blank_or_comment(line):
        i += 1
        continue
    ind = indent_of(line)
    if ind == 0:
        break
    if ind == job_indent and line.rstrip().endswith(":"):
        if current is not None:
            jobs.append((current[0], current[1], i))
        name = line.strip().rstrip(":")
        current = (name, i + 1)
    i += 1
if current is not None:
    jobs.append((current[0], current[1], i))

for name, start, end in jobs:
    is_reusable = 0
    has_timeout = 0
    timeout_value = "-"
    for k in range(start, end):
        line = lines[k]
        if is_blank_or_comment(line):
            continue
        if indent_of(line) != child_indent:
            continue
        key, _, val = line.strip().partition(":")
        key = key.strip()
        val = val.strip()
        if key == "uses":
            is_reusable = 1
        elif key == "timeout-minutes":
            has_timeout = 1
            timeout_value = val if val else "-"
    print(f"JOB\t{name}\t{is_reusable}\t{has_timeout}\t{timeout_value}")
PY
  )"

  if [[ -z "$report" ]]; then
    fail "$wf" "no jobs found — cannot verify timeouts"
    return
  fi

  while IFS=$'\t' read -r tag name is_reusable has_timeout value; do
    [[ "$tag" == "JOB" ]] || continue
    if [[ "$is_reusable" == "1" ]]; then
      # Reusable-call jobs must not declare timeout-minutes.
      if [[ "$has_timeout" == "1" ]]; then
        fail "$wf" "job '$name' calls a reusable workflow and must not declare timeout-minutes (set it in the called workflow's jobs)"
      else
        ok "$wf" "job '$name' is a reusable-workflow call (timeout belongs in the called workflow)"
      fi
      continue
    fi

    if [[ "$has_timeout" != "1" ]]; then
      fail "$wf" "job '$name' has no timeout-minutes — it inherits GitHub's 360-minute default"
      continue
    fi

    if [[ ! "$value" =~ ^[0-9]+$ ]]; then
      fail "$wf" "job '$name' timeout-minutes must be a positive integer (found: '$value')"
      continue
    fi
    if [[ "$value" -lt 1 || "$value" -gt "$MAX_TIMEOUT" ]]; then
      fail "$wf" "job '$name' timeout-minutes must be between 1 and $MAX_TIMEOUT (found: $value)"
      continue
    fi

    ok "$wf" "job '$name' declares timeout-minutes: $value"
  done <<< "$report"
}

for wf in "${WORKFLOWS[@]}"; do
  validate_workflow "$wf"
done

exit "$EXIT_CODE"
