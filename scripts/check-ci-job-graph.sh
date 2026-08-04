#!/usr/bin/env bash
# Validate the CI job dependency graph in .github/workflows/ci.yml (Issue #23).
#
# The workflow must declare explicit `needs:` relationships so that:
#   1. Foundational jobs (file layout validation) run before expensive jobs
#      (the Rust compile/test matrix), making re-runs and partial failures
#      predictable.
#   2. A single aggregator job `ci-required` fans in every gating job and
#      succeeds only when all of them succeed (or are cleanly skipped for
#      event types where a dependency is irrelevant, e.g. `security` on push).
#      Branch protection pins this one stable name as the required status.
#
# Rules enforced:
#   * Expected jobs are defined: validation, quality, security, shell-checks,
#     spell-check, ci-required.
#   * `quality` and `security` each declare `needs: [validation]` (ordering —
#     do not waste compute until the foundation check passes).
#   * `ci-required` lists every gating job in its `needs:` and uses
#     `if: always()` so it reports a deterministic result even on failure.
#   * `ci-required` inspects `needs.*.result` via an explicit success check —
#     otherwise `always()` would mask upstream failures.
#
# This validator is purpose-built: it scans `ci.yml` line-by-line rather than
# pulling in a YAML parser. Designed for reuse from BATS tests.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/check-harness.sh
source "$SCRIPT_DIR/lib/check-harness.sh"

usage() {
  cat <<'EOF'
Usage: check-ci-job-graph.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the CI workflow YAML file (default:
                    .github/workflows/ci.yml relative to repo root).
  -h, --help        Show this message.

Exits 0 when the workflow declares the expected job dependency graph, a
single aggregator job, and deterministic result semantics. Exits non-zero
with a descriptive message otherwise.
EOF
}

parse_check_args --workflow ".github/workflows/ci.yml" "$@"
WORKFLOW="$CHECK_TARGET"
check_require_file "$WORKFLOW"
check_subject "$WORKFLOW"

# Emit a JSON-ish report of each job's top-level keys (`needs`, `if`) using a
# minimal Python scanner — same approach as check-workflow-paths.sh.
report="$(
  python3 - "$WORKFLOW" <<'PY'
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as fh:
    lines = fh.read().splitlines()

def indent_of(line: str) -> int:
    return len(line) - len(line.lstrip(" "))

# Find the `jobs:` map. We expect it at column 0.
jobs_start = None
for i, line in enumerate(lines):
    if line.rstrip() == "jobs:":
        jobs_start = i + 1
        break

if jobs_start is None:
    print("ERROR\tno 'jobs:' key found")
    sys.exit(0)

# Determine job entries: keys at indent == 2 under `jobs:`.
job_indent = None
jobs = []  # list of (name, start, end)
i = jobs_start
while i < len(lines):
    line = lines[i]
    if not line.strip() or line.lstrip().startswith("#"):
        i += 1
        continue
    ind = indent_of(line)
    # A new top-level section at column 0 ends the jobs map.
    if ind == 0:
        break
    if job_indent is None:
        job_indent = ind
    if ind == job_indent and line.rstrip().endswith(":"):
        name = line.strip().rstrip(":")
        jobs.append([name, i, None])
    i += 1

# Fill in end indices.
for k, entry in enumerate(jobs):
    if k + 1 < len(jobs):
        entry[2] = jobs[k + 1][1]
    else:
        # End at next column-0 key or EOF.
        end = len(lines)
        for j in range(entry[1] + 1, len(lines)):
            if lines[j] and indent_of(lines[j]) == 0:
                end = j
                break
        entry[2] = end

for name, start, end in jobs:
    block = lines[start:end]
    needs_val = ""
    if_val = ""
    # Only inspect keys that are direct children of the job (indent ==
    # job_indent + 2). Nested `if:` inside steps must be ignored.
    child_indent = job_indent + 2
    k = 0
    while k < len(block):
        line = block[k]
        if not line.strip() or line.lstrip().startswith("#"):
            k += 1
            continue
        ind = indent_of(line)
        if ind != child_indent:
            k += 1
            continue
        s = line.strip()
        if s.startswith("needs:"):
            tail = s[len("needs:"):].strip()
            if tail:
                # inline form: `needs: [a, b]` or `needs: a`
                needs_val = tail
            else:
                # block-scalar list form
                collected = []
                m = k + 1
                while m < len(block):
                    nxt = block[m]
                    if not nxt.strip():
                        m += 1
                        continue
                    ni = indent_of(nxt)
                    if ni <= child_indent:
                        break
                    if nxt.lstrip().startswith("- "):
                        collected.append(nxt.strip()[2:].strip())
                    m += 1
                needs_val = "[" + ", ".join(collected) + "]"
        elif s.startswith("if:"):
            if_val = s[len("if:"):].strip()
        k += 1
    print(f"JOB\t{name}\t{needs_val}\t{if_val}")
PY
)"

# Parse the report. macOS bash 3.2 lacks associative arrays, so look up each
# job's metadata by greppping the TSV report on demand.
if [[ "$report" == *$'\t'*"ERROR"* || "${report%%$'\t'*}" == "ERROR" ]]; then
  fail "${report#ERROR$'\t'}"
  exit 1
fi

# Helper: print the `needs` field for a named job (empty string if absent).
lookup_needs() {
  local target="$1"
  awk -F '\t' -v name="$target" '$1=="JOB" && $2==name { print $3 }' <<< "$report"
}

# Helper: print the `if` field for a named job (empty string if absent).
lookup_if() {
  local target="$1"
  awk -F '\t' -v name="$target" '$1=="JOB" && $2==name { print $4 }' <<< "$report"
}

has_job() {
  local target="$1"
  awk -F '\t' -v name="$target" 'BEGIN { found=1 } $1=="JOB" && $2==name { found=0 } END { exit found }' <<< "$report"
}

# 1. Expected gating jobs are defined.
EXPECTED=(validation quality security shell-checks spell-check ci-required)
for job in "${EXPECTED[@]}"; do
  if has_job "$job"; then
    ok "job '$job' defined"
  else
    fail "expected job '$job' is not defined"
  fi
done

# 2. `quality` depends on `validation` (foundation runs first).
quality_needs="$(lookup_needs quality)"
if [[ "$quality_needs" == *"validation"* ]]; then
  ok "job 'quality' declares needs on 'validation'"
else
  fail "job 'quality' must declare needs: [validation] so foundation runs first"
fi

# 3. `security` depends on `validation`.
security_needs="$(lookup_needs security)"
if [[ "$security_needs" == *"validation"* ]]; then
  ok "job 'security' declares needs on 'validation'"
else
  fail "job 'security' must declare needs: [validation]"
fi

# 4. `ci-required` lists every gating job in its needs.
GATING=(validation quality security shell-checks spell-check)
ci_needs="$(lookup_needs ci-required)"
for g in "${GATING[@]}"; do
  if [[ "$ci_needs" == *"$g"* ]]; then
    ok "aggregator 'ci-required' needs '$g'"
  else
    fail "aggregator 'ci-required' must list '$g' in needs (found: '$ci_needs')"
  fi
done

# 5. `ci-required` uses `if: always()` so it reports a result even on upstream
#    failure — otherwise the check would be "skipped" and branch protection
#    would wait forever.
ci_if="$(lookup_if ci-required)"
if [[ "$ci_if" == *"always()"* ]]; then
  ok "aggregator 'ci-required' uses if: always()"
else
  fail "aggregator 'ci-required' must set 'if: always()' for deterministic result"
fi

# 6. The aggregator's run: block must check needs.*.result explicitly —
#    otherwise `always()` would pass the job even if an upstream job failed.
if grep -qE 'needs\.[A-Za-z0-9_-]+\.result' "$WORKFLOW"; then
  ok "aggregator checks needs.*.result explicitly"
else
  fail "aggregator must inspect 'needs.<job>.result' — always() alone masks failures"
fi

exit "$EXIT_CODE"
