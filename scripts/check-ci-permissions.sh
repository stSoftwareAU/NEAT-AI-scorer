#!/usr/bin/env bash
# Validate the least-privilege GITHUB_TOKEN scope in .github/workflows/ci.yml
# (Issue #155).
#
# Background: a workflow with no `permissions:` block inherits the
# repository/organisation default token scope, which can be read/write across
# many scopes. The `ci.yml` jobs only read the checked-out code and run
# build/lint/test steps, so a broad token violates least privilege and widens
# the blast radius if any step (or a dependency it installs) is compromised.
#
# Rules enforced:
#   1. A workflow-level (top-level, column 0) `permissions:` block exists.
#   2. That default is read-only: it grants `contents: read` and declares no
#      `*: write` scope at the workflow level.
#   3. The `security` job — which calls the reusable `security.yml` — declares
#      its own job-level `permissions:` granting the scopes the reusable
#      workflow needs (`checks: write`, `pull-requests: write`, and
#      `contents: read`). This is required because a called workflow's token can
#      only be narrowed, never elevated, along the caller chain — without the
#      job-level grant the reusable workflow's own `checks: write` /
#      `pull-requests: write` would be clamped away by the read-only workflow
#      default and the security annotations / dependency-review PR comment would
#      silently fail. `pull-requests: write` (not `issues: write`) is the scope
#      the dependency-review action needs to post its `comment-summary-in-pr`
#      summary; the audit-check action only files issues on non-PR events, which
#      this PR-gated caller never triggers (Issue #398).
#
# The script takes a single optional `--workflow PATH` argument so BATS tests
# can exercise it against fixtures. With no argument it validates
# `.github/workflows/ci.yml` relative to the repo root.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/check-harness.sh
source "$SCRIPT_DIR/lib/check-harness.sh"

usage() {
  cat <<'EOF'
Usage: check-ci-permissions.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the CI workflow YAML file (default:
                    .github/workflows/ci.yml relative to the repo root).
  -h, --help        Show this message.

Exits 0 when the workflow declares a read-only top-level permissions default
and grants the security job the scopes its reusable workflow needs. Exits
non-zero with a descriptive message otherwise.
EOF
}

parse_check_args --workflow ".github/workflows/ci.yml" "$@"
WORKFLOW="$CHECK_TARGET"
check_require_file "$WORKFLOW"
check_subject "$WORKFLOW"

# Emit a TSV report describing the workflow-level permissions block and the
# `security` job's job-level permissions block. A minimal Python scanner walks
# the YAML by indentation rather than pulling in a YAML parser — the same
# approach as check-ci-job-graph.sh.
#
# Report lines:
#   TOPLEVEL_PERMS\t<0|1>          workflow-level permissions block present
#   TOP\t<key>\t<value>           a scope declared in the top-level block
#   SECURITY_JOB\t<0|1>            a `security` job is defined
#   SECURITY_PERMS\t<0|1>         the security job declares permissions
#   SEC\t<key>\t<value>          a scope declared in the security job block
report="$(
  python3 - "$WORKFLOW" <<'PY'
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as fh:
    lines = fh.read().splitlines()

def indent_of(line):
    return len(line) - len(line.lstrip(" "))

def is_blank_or_comment(line):
    s = line.strip()
    return (not s) or s.startswith("#")

def collect_block(start, parent_indent):
    """Collect `key: value` children whose indent is exactly parent_indent+2,
    stopping at the first line indented back to <= parent_indent."""
    child_indent = parent_indent + 2
    out = []
    i = start
    while i < len(lines):
        line = lines[i]
        if is_blank_or_comment(line):
            i += 1
            continue
        ind = indent_of(line)
        if ind <= parent_indent:
            break
        if ind == child_indent and ":" in line:
            key, _, val = line.strip().partition(":")
            out.append((key.strip(), val.strip()))
        i += 1
    return out

# --- Workflow-level permissions (indent 0) ---
top_perms_present = False
top_perms = []
for i, line in enumerate(lines):
    if indent_of(line) == 0 and line.strip().rstrip() == "permissions:":
        top_perms_present = True
        top_perms = collect_block(i + 1, 0)
        break

print(f"TOPLEVEL_PERMS\t{1 if top_perms_present else 0}")
for k, v in top_perms:
    print(f"TOP\t{k}\t{v}")

# --- Locate the jobs map and the `security` job block ---
jobs_start = None
for i, line in enumerate(lines):
    if indent_of(line) == 0 and line.strip() == "jobs:":
        jobs_start = i + 1
        break

security_job_present = False
security_perms_present = False
security_perms = []

if jobs_start is not None:
    # Job entries are keys at the first indent level under `jobs:`.
    job_indent = None
    sec_start = None
    sec_end = len(lines)
    i = jobs_start
    while i < len(lines):
        line = lines[i]
        if is_blank_or_comment(line):
            i += 1
            continue
        ind = indent_of(line)
        if ind == 0:
            break
        if job_indent is None:
            job_indent = ind
        if ind == job_indent and line.rstrip().endswith(":"):
            name = line.strip().rstrip(":")
            if name == "security":
                security_job_present = True
                sec_start = i + 1
                # find end: next line at indent <= job_indent
                for j in range(i + 1, len(lines)):
                    if is_blank_or_comment(lines[j]):
                        continue
                    if indent_of(lines[j]) <= job_indent:
                        sec_end = j
                        break
                break
        i += 1

    if security_job_present and sec_start is not None:
        child_indent = job_indent + 2
        k = sec_start
        while k < sec_end:
            line = lines[k]
            if is_blank_or_comment(line):
                k += 1
                continue
            if indent_of(line) == child_indent and line.strip().rstrip() == "permissions:":
                security_perms_present = True
                security_perms = collect_block(k + 1, child_indent)
                break
            k += 1

print(f"SECURITY_JOB\t{1 if security_job_present else 0}")
print(f"SECURITY_PERMS\t{1 if security_perms_present else 0}")
for k, v in security_perms:
    print(f"SEC\t{k}\t{v}")
PY
)"

# --- Helpers to query the TSV report ---
field() {
  # field <TAG> -> prints the single value column for a 2-column tag line
  awk -F '\t' -v tag="$1" '$1==tag { print $2 }' <<< "$report"
}
scope_value() {
  # scope_value <TAG> <key> -> prints the value for a scope under TAG (TOP|SEC)
  awk -F '\t' -v tag="$1" -v key="$2" '$1==tag && $2==key { print $3 }' <<< "$report"
}
write_scopes() {
  # write_scopes <TAG> -> prints "key" for each scope whose value is "write"
  awk -F '\t' -v tag="$1" '$1==tag && $3=="write" { print $2 }' <<< "$report"
}

# 1. Workflow-level permissions block present.
if [[ "$(field TOPLEVEL_PERMS)" == "1" ]]; then
  ok "workflow-level permissions block present"
else
  fail "no workflow-level 'permissions:' block — token inherits the broad repository default"
fi

# 2. Top-level default is read-only.
if [[ "$(scope_value TOP contents)" == "read" ]]; then
  ok "top-level default grants contents: read"
else
  fail "top-level permissions must grant 'contents: read'"
fi

top_writes="$(write_scopes TOP)"
if [[ -z "$top_writes" ]]; then
  ok "top-level default declares no write scopes (read-only)"
else
  fail "top-level permissions must not grant write scopes (found: $(echo "$top_writes" | tr '\n' ' '))"
fi

# 3. The security job grants the reusable workflow its required scopes.
if [[ "$(field SECURITY_JOB)" == "1" ]]; then
  ok "security job defined"

  if [[ "$(field SECURITY_PERMS)" == "1" ]]; then
    ok "security job declares a job-level permissions block"
  else
    fail "security job must declare job-level 'permissions:' — the reusable security.yml needs checks/issues write, and a read-only default would clamp them away"
  fi

  for scope in checks pull-requests; do
    if [[ "$(scope_value SEC "$scope")" == "write" ]]; then
      ok "security job grants $scope: write"
    else
      fail "security job must grant '$scope: write' for the reusable security.yml"
    fi
  done

  if [[ "$(scope_value SEC contents)" == "read" ]]; then
    ok "security job grants contents: read"
  else
    fail "security job must grant 'contents: read' for the reusable security.yml"
  fi
else
  fail "no 'security' job found — cannot verify its token scope"
fi

exit "$EXIT_CODE"
