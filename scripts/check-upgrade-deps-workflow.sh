#!/usr/bin/env bash
# Validate the weekly `upgrade-dependencies.yml` workflow (Issue #90).
#
# PR #89 was raised by this workflow with a misleading body and three
# stray commits beyond the legitimate Cargo.lock bump: the workflow
# log files `upgrade-dry-run.txt` and `upgrade.log`, and a gitlink for
# the sibling NEAT-AI-core checkout. The reader could not tell from
# the PR description whether any dependency had actually been bumped.
#
# This validator enforces three rules on the workflow YAML so the same
# regression cannot recur:
#
#   1. `cargo upgrade` logs are written OUTSIDE the repository — to
#      `${{ runner.temp }}` or `$RUNNER_TEMP` — so they cannot be
#      committed by accident.
#   2. The `peter-evans/create-pull-request` step declares `add-paths:`
#      and lists only Cargo manifest/lock paths. This stops the
#      action staging the NEAT-AI-core sibling clone or any other
#      runner debris.
#   3. The PR body includes a `git diff` over the Cargo files so the
#      reviewer can immediately see WHAT changed — not just the
#      "aborting upgrade due to dry run" line that misled the reader
#      of PR #89.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-upgrade-deps-workflow.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the upgrade-dependencies workflow YAML
                    (default: .github/workflows/upgrade-dependencies.yml
                    relative to repo root).
  -h, --help        Show this message.

Exits 0 when the workflow satisfies every rule documented in the
script header; non-zero with a descriptive message otherwise.
EOF
}

WORKFLOW=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --workflow)
      WORKFLOW="${2:-}"
      if [[ -z "$WORKFLOW" ]]; then
        echo "Error: --workflow requires an argument" >&2
        usage >&2
        exit 2
      fi
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
  WORKFLOW="$SCRIPT_DIR/../.github/workflows/upgrade-dependencies.yml"
fi

if [[ ! -f "$WORKFLOW" ]]; then
  echo "Workflow file not found: $WORKFLOW" >&2
  exit 2
fi

EXIT_CODE=0
fail() {
  echo "FAIL $WORKFLOW: $*" >&2
  EXIT_CODE=1
}
ok() {
  echo "OK   $WORKFLOW: $*"
}

# --- Rule 1: upgrade logs must NOT be tee'd / redirected into the repo
# root. The PR #89 regression came from `tee upgrade-dry-run.txt` and
# `tee upgrade.log` (both relative to $GITHUB_WORKSPACE), which the
# create-pull-request action then staged. Allow any path that begins
# with `${RUNNER_TEMP}`, `${{ runner.temp }}`, `/tmp/`, or similar.
if grep -nE 'tee[[:space:]]+(upgrade-dry-run\.txt|upgrade\.log)([[:space:]]|$)' "$WORKFLOW" >/dev/null \
  || grep -nE '(>|>>)[[:space:]]*(upgrade-dry-run\.txt|upgrade\.log)([[:space:]]|$)' "$WORKFLOW" >/dev/null; then
  fail "cargo upgrade logs written to the repo root — redirect to \${RUNNER_TEMP} or \${{ runner.temp }} so they cannot be committed"
else
  ok "cargo upgrade logs are not redirected into the repo root"
fi

# Positive evidence: if the workflow tees logs, the destination should
# reference RUNNER_TEMP / runner.temp. This catches the "tee
# ./upgrade.log" / "tee logs/upgrade.log" variants without enumerating
# every possible safe location.
if grep -qE 'cargo[[:space:]]+upgrade' "$WORKFLOW"; then
  if grep -qE 'tee[[:space:]]+["'\''$]*\{?\{?[[:space:]]*(RUNNER_TEMP|runner\.temp)' "$WORKFLOW"; then
    ok "cargo upgrade logs are tee'd under RUNNER_TEMP"
  elif grep -qE 'cargo[[:space:]]+upgrade[^|]*\|[[:space:]]*tee' "$WORKFLOW"; then
    # tee is used but the destination did not match RUNNER_TEMP — only
    # warn if no other rule already caught the issue.
    if [[ "$EXIT_CODE" -eq 0 ]]; then
      fail "cargo upgrade output is tee'd but not to RUNNER_TEMP — use \"\${RUNNER_TEMP}/upgrade.log\" instead"
    fi
  fi
fi

# --- Rule 2: peter-evans/create-pull-request must declare add-paths,
# and add-paths may only contain Cargo manifest/lock entries. We parse
# the YAML lightly: find the block starting at `uses: peter-evans/
# create-pull-request`, then within that step look for the `with:`
# block and an `add-paths:` key whose values are all Cargo paths.
PYTHON_BIN="${PYTHON_BIN:-python3}"
if ! command -v "$PYTHON_BIN" >/dev/null 2>&1; then
  fail "python3 not available — required to validate add-paths"
else
  PY_RESULT="$(
    "$PYTHON_BIN" - "$WORKFLOW" <<'PY'
import re
import sys
from pathlib import Path

path = Path(sys.argv[1])
text = path.read_text()

# Find every peter-evans/create-pull-request usage. For each, locate the
# enclosing `with:` block and inspect its `add-paths` list.
uses_re = re.compile(
    r'^([ \t]*)-?\s*uses:\s*peter-evans/create-pull-request',
    re.MULTILINE,
)
lines = text.splitlines()
n = len(lines)
problems = []
found_action = False

def step_indent(line: str) -> int:
    return len(line) - len(line.lstrip(" "))

for m in uses_re.finditer(text):
    found_action = True
    # Locate the line number of the match.
    line_no = text.count("\n", 0, m.start())
    # `uses:` and its sibling step keys (`with:`, `name:`, `if:`, ...)
    # share the same indent — they are all keys of the same step map.
    indent = step_indent(lines[line_no])
    # Find the `with:` line at the same indent within this step.
    with_idx = None
    i = line_no + 1
    while i < n:
        line = lines[i]
        stripped = line.lstrip(" ")
        if stripped == "" or stripped.startswith("#"):
            i += 1
            continue
        cur_indent = step_indent(line)
        if cur_indent < indent:
            # Dedented out of the step.
            break
        if cur_indent == indent:
            # Sibling key OR start of next step (the `-` list marker
            # sits at indent-2, so anything at this indent that starts
            # with `-` means a new step). Either way we stop after
            # checking for `with:`.
            if stripped.startswith("- "):
                break
            if stripped.startswith("with:"):
                with_idx = i
                break
        i += 1

    if with_idx is None:
        problems.append("create-pull-request step is missing `with:` (cannot declare add-paths)")
        continue

    with_indent = step_indent(lines[with_idx])
    # Scan the with-block for an add-paths key.
    add_paths_idx = None
    j = with_idx + 1
    while j < n:
        line = lines[j]
        stripped = line.lstrip(" ")
        if stripped == "" or stripped.startswith("#"):
            j += 1
            continue
        cur_indent = step_indent(line)
        if cur_indent <= with_indent:
            break
        if cur_indent == with_indent + 2 and stripped.startswith("add-paths"):
            add_paths_idx = j
            break
        j += 1

    if add_paths_idx is None:
        problems.append("create-pull-request step does not set `add-paths:` (would stage every changed file)")
        continue

    # Collect the list values. Support inline `add-paths: [a, b]` and
    # block scalar `add-paths: |` / `add-paths:\n  - a`.
    key_line = lines[add_paths_idx]
    paths = []
    key_value = key_line.split(":", 1)[1].strip()
    if key_value.startswith("[") and key_value.endswith("]"):
        inner = key_value[1:-1]
        paths = [p.strip().strip('"').strip("'") for p in inner.split(",") if p.strip()]
    else:
        # Block scalar OR YAML list.
        k = add_paths_idx + 1
        block_indent = None
        # Block scalar marker (|, |-, >, >-) means subsequent indented
        # lines are literal entries — one per line.
        is_block_scalar = key_value in {"|", "|-", "|+", ">", ">-", ">+"}
        while k < n:
            line = lines[k]
            stripped = line.lstrip(" ")
            cur_indent = step_indent(line)
            if stripped == "" or stripped.startswith("#"):
                k += 1
                continue
            if block_indent is None:
                if cur_indent <= step_indent(key_line):
                    break
                block_indent = cur_indent
            if cur_indent < block_indent:
                break
            if is_block_scalar:
                paths.append(stripped)
            else:
                # YAML list item.
                if stripped.startswith("- "):
                    paths.append(stripped[2:].strip().strip('"').strip("'"))
                else:
                    paths.append(stripped.strip().strip('"').strip("'"))
            k += 1

    allowed = {"Cargo.toml", "**/Cargo.toml", "Cargo.lock", "*/Cargo.toml"}
    extras = [p for p in paths if p not in allowed]
    if extras:
        problems.append("add-paths contains unexpected entries (only Cargo.toml/Cargo.lock allowed): " + ", ".join(extras))
    elif not paths:
        problems.append("add-paths is empty — must list Cargo.toml and Cargo.lock")

if not found_action:
    problems.append("workflow does not use peter-evans/create-pull-request — cannot enforce add-paths")

print("\n".join(problems))
PY
  )"
  if [[ -n "$PY_RESULT" ]]; then
    while IFS= read -r problem; do
      [[ -n "$problem" ]] && fail "$problem"
    done <<<"$PY_RESULT"
  else
    ok "create-pull-request restricts add-paths to Cargo manifest/lock entries"
  fi
fi

# --- Rule 3: the PR body must include a `git diff` over the Cargo
# files so reviewers can immediately tell what was bumped. Accept any
# `git diff` invocation that mentions Cargo.lock or Cargo.toml.
if grep -qE 'git[[:space:]]+diff[^#\r\n]*Cargo\.(lock|toml)' "$WORKFLOW"; then
  ok "PR body / summary step runs git diff over Cargo files"
else
  fail "PR body does not run 'git diff … Cargo.(lock|toml)' — reviewer cannot see the actual bumps"
fi

# --- Rule 4 (Issue #101): the scheduled bump MUST be routed through
# bump-deps.sh so the VIBE_BUMP_QUARANTINE_HOURS gate (default 24h) is
# applied. Calling `cargo upgrade` directly bypasses the gate and lets
# a freshly-published malicious crates.io version reach the
# auto-generated PR within minutes of publication.
if grep -qE '(^|[^[:alnum:]_/.-])(\./)?bump-deps\.sh([[:space:]]|$)' "$WORKFLOW"; then
  ok "scheduled bump is routed through bump-deps.sh (quarantine gate)"
else
  fail "workflow invokes 'cargo upgrade' without bump-deps.sh — the quarantine gate is bypassed (Issue #101)"
fi

exit "$EXIT_CODE"
