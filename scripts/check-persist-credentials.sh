#!/usr/bin/env bash
# Validate that every `actions/checkout` step in the guarded workflows
# disables credential persistence (Issues #388, #389, #378).
#
# Background: by default `actions/checkout` writes the workflow's GITHUB_TOKEN
# into `.git/config` as an auth header. Any later step in the same job — a
# compromised dependency, an injected script, a supply-chain payload — can then
# read that token and act as it. The `security` job only reads the checked-out
# code and runs cargo-audit / dependency-review; it never pushes back and never
# fetches a private submodule, so the persisted credential is pure blast radius.
# Setting `persist-credentials: false` keeps the token off disk.
#
# Rule enforced: for the target workflow, every checkout step must either
#   1. declare `persist-credentials: false` in its `with:` block, or
#   2. carry an inline `# best-practice-ignore: BP-PERSIST-CREDS ...` comment
#      documenting why the credential is genuinely required.
#
# The script takes an optional `--workflow PATH` argument so BATS tests can
# exercise it against fixtures. With no argument it validates every guarded
# workflow (`security.yml`, `semgrep.yml`, `cargo-quality.yml`) relative to the
# repo root.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-persist-credentials.sh [--workflow PATH]

Options:
  --workflow PATH   Path to a single workflow YAML file to validate. When
                    omitted, every guarded workflow under
                    .github/workflows/ (ci.yml, security.yml, semgrep.yml,
                    cargo-quality.yml) is checked.
  -h, --help        Show this message.

Exits 0 when every actions/checkout step disables credential persistence (or
carries a documented best-practice-ignore). Exits non-zero with a descriptive
message otherwise.
EOF
}

WORKFLOW=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --workflow)
      if [[ $# -lt 2 ]]; then
        echo "Missing value for --workflow" >&2
        usage >&2
        exit 2
      fi
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

# Build the list of workflows to validate. An explicit --workflow overrides the
# default set; otherwise every guarded workflow is checked in one run
# (ci.yml is guarded per Issue #378).
WORKFLOWS=()
if [[ -n "$WORKFLOW" ]]; then
  WORKFLOWS=("$WORKFLOW")
else
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  WORKFLOWS=(
    "$SCRIPT_DIR/../.github/workflows/ci.yml"
    "$SCRIPT_DIR/../.github/workflows/security.yml"
    "$SCRIPT_DIR/../.github/workflows/semgrep.yml"
    "$SCRIPT_DIR/../.github/workflows/cargo-quality.yml"
  )
fi

EXIT_CODE=0

validate_workflow() {
  local WORKFLOW="$1"

  if [[ ! -f "$WORKFLOW" ]]; then
    echo "Workflow file not found: $WORKFLOW" >&2
    EXIT_CODE=2
    return
  fi

  fail() {
    echo "FAIL $WORKFLOW: $*" >&2
    EXIT_CODE=1
  }
  ok() {
    echo "OK   $WORKFLOW: $*"
  }

# Emit one report line per checkout step found:
#   STEP\t<line>\t<persist_ok|persist_missing|ignored>
# A minimal indentation-aware scanner walks the YAML rather than pulling in a
# YAML parser — the same approach as check-ci-permissions.sh.
report="$(
  python3 - "$WORKFLOW" <<'PY'
import re
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as fh:
    lines = fh.read().splitlines()

def indent_of(line):
    return len(line) - len(line.lstrip(" "))

def is_blank_or_comment(line):
    s = line.strip()
    return (not s) or s.startswith("#")

CHECKOUT = re.compile(r"uses:\s*actions/checkout(@|\s|$)")
IGNORE = re.compile(r"#\s*best-practice-ignore:\s*BP-PERSIST-CREDS")

for i, line in enumerate(lines):
    if not CHECKOUT.search(line):
        continue

    step_indent = indent_of(line)

    # A documented ignore may sit on the `uses:` line itself or on a comment
    # line immediately above it (skipping intervening blank lines).
    ignored = bool(IGNORE.search(line))
    if not ignored:
        j = i - 1
        while j >= 0 and lines[j].strip() == "":
            j -= 1
        if j >= 0 and IGNORE.search(lines[j]):
            ignored = True

    # Scan the rest of this step for the persist flag. Sibling step keys
    # (`with:`, `env:`) align with the `uses:` key at step_indent; the flag
    # itself is a child of `with:` at step_indent+2. Stop at the first line
    # dedented below step_indent — that is the next step or the steps-list end.
    persist_ok = False
    k = i + 1
    while k < len(lines):
        cur = lines[k]
        if is_blank_or_comment(cur):
            k += 1
            continue
        if indent_of(cur) < step_indent:
            break
        if re.search(r"persist-credentials:\s*false\b", cur):
            persist_ok = True
            break
        k += 1

    if persist_ok:
        state = "persist_ok"
    elif ignored:
        state = "ignored"
    else:
        state = "persist_missing"
    print(f"STEP\t{i + 1}\t{state}")
PY
)"

  checkout_count="$(grep -c $'^STEP\t' <<<"$report" || true)"
  if [[ "$checkout_count" -eq 0 ]]; then
    fail "no 'actions/checkout' step found — expected at least one to validate"
    return
  fi

  while IFS=$'\t' read -r tag lineno state; do
    [[ "$tag" == "STEP" ]] || continue
    case "$state" in
      persist_ok)
        ok "checkout at line $lineno sets persist-credentials: false"
        ;;
      ignored)
        ok "checkout at line $lineno carries a documented BP-PERSIST-CREDS ignore"
        ;;
      *)
        fail "checkout at line $lineno must set 'persist-credentials: false' (or document a BP-PERSIST-CREDS ignore) so the GITHUB_TOKEN is not written to .git/config"
        ;;
    esac
  done <<<"$report"
}

for wf in "${WORKFLOWS[@]}"; do
  validate_workflow "$wf"
done

exit "$EXIT_CODE"
