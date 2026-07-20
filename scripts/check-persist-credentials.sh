#!/usr/bin/env bash
# Validate that credential-free `actions/checkout` steps set
# `persist-credentials: false` (Issue #380).
#
# Background: by default `actions/checkout` writes the workflow's `GITHUB_TOKEN`
# into `.git/config` as an auth header. Any later step in the same job — a
# compromised dependency, an injected script — can then read that token and act
# as it. A job that neither pushes back to the repository nor fetches an
# additional repository (private submodule / sibling clone) does not need the
# persisted credential, so leaving it on disk only widens the blast radius of a
# compromised step. Such a job MUST set `persist-credentials: false`.
#
# Rule enforced, per job in each workflow:
#   * A job whose ONLY `actions/checkout` step is a single checkout of the
#     current repository MUST set `persist-credentials: false` on that step.
#   * A job that runs MORE THAN ONE checkout (it fetches an additional
#     repository — e.g. the NEAT-AI-core sibling clone) is exempt: the extra
#     checkout is a static sign the job legitimately needs a credential.
#   * A single-checkout job may opt out with an in-source
#     `# best-practice-ignore: BP-PERSIST-CREDS <reason>` comment above the
#     `uses:` line when it genuinely pushes back or fetches a private submodule.
#
# The script accepts zero or more `--workflow PATH` arguments so BATS tests can
# exercise it against fixtures. With no argument it validates `ci.yml` — the
# file this rule was introduced for (Issue #380). Sibling workflows carry their
# own `BP-PERSIST-CREDS` audit findings; broadening this gate to every workflow
# is deferred until those land, so an unfixed sibling cannot fail this check.
# Point `--workflow` at any file to validate it directly.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-persist-credentials.sh [--workflow PATH]...

Options:
  --workflow PATH   Path to a workflow YAML file to validate. May be repeated.
                    With no --workflow argument, .github/workflows/ci.yml
                    (relative to the repo root) is checked — see the header note
                    on scope (Issue #380).
  -h, --help        Show this message.

Exits 0 when every credential-free single-checkout job sets
`persist-credentials: false` (or documents a suppression). Exits non-zero with a
descriptive message otherwise.
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
  WORKFLOWS+=("$SCRIPT_DIR/../.github/workflows/ci.yml")
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

validate_workflow() {
  local wf="$1"

  if [[ ! -f "$wf" ]]; then
    echo "Workflow file not found: $wf" >&2
    EXIT_CODE=1
    return
  fi

  # Emit a TSV report, one line per job:
  #   JOB\t<name>\t<checkout_count>\t<persist_false 0|1>\t<suppressed 0|1>
  # persist_false / suppressed describe the FIRST checkout step in the job (the
  # only one that matters for a single-checkout job). A minimal
  # indentation-based scanner walks the YAML rather than pulling in a YAML
  # parser — the same approach as check-workflow-timeouts.sh.
  local report
  report="$(
    python3 - "$wf" <<'PY'
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as fh:
    lines = fh.read().splitlines()

def indent_of(line):
    return len(line) - len(line.lstrip(" "))

def is_blank(line):
    return not line.strip()

def is_comment(line):
    return line.strip().startswith("#")

def is_blank_or_comment(line):
    return is_blank(line) or is_comment(line)

# Locate the top-level `jobs:` map.
jobs_start = None
for i, line in enumerate(lines):
    if indent_of(line) == 0 and line.strip() == "jobs:":
        jobs_start = i + 1
        break

if jobs_start is None:
    sys.exit(0)

# Determine the job-key indent (first non-blank/comment line under `jobs:`).
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

CHECKOUT_MARKER = "actions/checkout"

def uses_checkout(line):
    key, sep, val = line.strip().partition(":")
    if key.strip().lstrip("- ").strip() != "uses":
        return False
    return CHECKOUT_MARKER in val

for name, start, end in jobs:
    # Find the `steps:` block and its list-item indent.
    steps_indent = None
    steps_line = None
    for k in range(start, end):
        line = lines[k]
        if is_blank_or_comment(line):
            continue
        if line.strip() == "steps:":
            steps_indent = indent_of(line)
            steps_line = k
            break

    checkout_count = 0
    first_persist_false = 0
    first_suppressed = 0

    if steps_line is not None:
        # List items live at steps_indent + 2 and begin with "- ".
        item_indent = steps_indent + 2
        # Gather step-item start lines.
        item_starts = []
        for k in range(steps_line + 1, end):
            line = lines[k]
            if is_blank_or_comment(line):
                continue
            ind = indent_of(line)
            if ind < item_indent:
                break
            if ind == item_indent and line.lstrip().startswith("- "):
                item_starts.append(k)
        item_starts.append(end)  # sentinel upper bound

        for idx in range(len(item_starts) - 1):
            s = item_starts[idx]
            e = item_starts[idx + 1]
            block = lines[s:e]
            is_checkout = any(uses_checkout(bl) for bl in block if not is_comment(bl))
            if not is_checkout:
                continue
            checkout_count += 1
            persist_false = any(
                bl.strip().replace(" ", "").lower() == "persist-credentials:false"
                for bl in block
                if not is_comment(bl)
            )
            # Suppression comment: within the step block, or on the comment
            # line(s) immediately above the step item.
            suppressed = any(
                is_comment(bl) and "best-practice-ignore" in bl and "BP-PERSIST-CREDS" in bl
                for bl in block
            )
            if not suppressed:
                j = s - 1
                while j >= start and (is_blank(lines[j]) or is_comment(lines[j])):
                    if is_comment(lines[j]) and "best-practice-ignore" in lines[j] and "BP-PERSIST-CREDS" in lines[j]:
                        suppressed = True
                        break
                    j -= 1
            if checkout_count == 1:
                first_persist_false = 1 if persist_false else 0
                first_suppressed = 1 if suppressed else 0

    print(f"JOB\t{name}\t{checkout_count}\t{first_persist_false}\t{first_suppressed}")
PY
  )"

  if [[ -z "$report" ]]; then
    # A workflow with no jobs (e.g. a reusable-only fragment) is not our concern.
    ok "$wf" "no jobs with checkout steps to validate"
    return
  fi

  while IFS=$'\t' read -r tag name checkout_count persist_false suppressed; do
    [[ "$tag" == "JOB" ]] || continue

    if [[ "$checkout_count" -eq 0 ]]; then
      ok "$wf" "job '$name' has no checkout step"
      continue
    fi

    if [[ "$checkout_count" -gt 1 ]]; then
      # Fetches an additional repository — a static sign it needs a credential.
      ok "$wf" "job '$name' runs $checkout_count checkouts (fetches an additional repository) — exempt"
      continue
    fi

    # Single checkout: must harden or document a suppression.
    if [[ "$persist_false" == "1" ]]; then
      ok "$wf" "job '$name' single checkout sets persist-credentials: false"
      continue
    fi
    if [[ "$suppressed" == "1" ]]; then
      ok "$wf" "job '$name' single checkout keeps credentials with a documented BP-PERSIST-CREDS suppression"
      continue
    fi

    fail "$wf" "job '$name' single checkout must set 'persist-credentials: false' (or document a '# best-practice-ignore: BP-PERSIST-CREDS <reason>' suppression)"
  done <<< "$report"
}

for wf in "${WORKFLOWS[@]}"; do
  validate_workflow "$wf"
done

exit "$EXIT_CODE"
