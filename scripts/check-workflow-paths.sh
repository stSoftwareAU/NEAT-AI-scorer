#!/usr/bin/env bash
# Validate the NEAT-AI-core checkout path strategy across all workflows (Issue #18).
#
# GitHub Actions requires that `actions/checkout`'s `path:` value is a subpath
# of `$GITHUB_WORKSPACE`. A path like `../NEAT-AI-core` resolves outside the
# workspace directory and fails with "Repository path '…' is not under '…'".
#
# This helper scans `.github/workflows/*.yml` for any `actions/checkout` step
# that targets `stSoftwareAU/NEAT-AI-core` and enforces two rules:
#
#   1. The `path:` value (when present) must NOT start with `..` or `/` — it
#      must be an in-workspace relative path.
#   2. Any workflow that checks out NEAT-AI-core in-workspace must also run a
#      "Link NEAT-AI-core sibling path" step so the Cargo path dependency
#      (`../../NEAT-AI-core/neat-core` from `rust_scorer/Cargo.toml`) resolves.
#
# Designed for reuse from BATS tests and `quality.sh`.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-workflow-paths.sh [--workflows DIR]

Options:
  --workflows DIR   Directory containing workflow YAML files (default:
                    .github/workflows relative to the repo root).
  -h, --help        Show this message.

Exits 0 when every workflow that checks out stSoftwareAU/NEAT-AI-core uses an
in-workspace `path:` AND contains a sibling-path link step. Exits non-zero
with a descriptive message otherwise.
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

EXIT_CODE=0
FOUND_ANY=0

# Iterate .yml and .yaml workflow files; python3 parses each one and emits
# findings as tab-separated records to stdout.
while IFS= read -r workflow; do
  [[ -f "$workflow" ]] || continue
  FOUND_ANY=1

  findings="$(
    python3 - "$workflow" <<'PY'
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as fh:
    lines = fh.read().splitlines()

# Minimal, purpose-built scanner: we do not need a full YAML parser — we only
# need to locate "uses: actions/checkout" blocks and inspect the `with:` map.
# A full parser would pull in PyYAML which is not a baseline dependency.

def indent_of(line: str) -> int:
    return len(line) - len(line.lstrip(" "))

checkout_blocks = []  # list of (start_index, with_indent_or_None)
i = 0
while i < len(lines):
    stripped = lines[i].lstrip()
    if stripped.startswith("- ") and "uses:" in stripped and "actions/checkout" in stripped:
        checkout_blocks.append(i)
    elif stripped.startswith("uses:") and "actions/checkout" in stripped:
        checkout_blocks.append(i)
    i += 1

findings = []
for start in checkout_blocks:
    # Determine the block's dedent boundary: walk forward until indentation
    # drops back to the step's own indent or less, then stop.
    step_indent = indent_of(lines[start])
    end = start + 1
    while end < len(lines):
        line = lines[end]
        if not line.strip():
            end += 1
            continue
        if indent_of(line) <= step_indent and not line.lstrip().startswith("- "):
            # same-level key under the step (name:, with:, env:, run:, etc.)
            end += 1
            continue
        if indent_of(line) <= step_indent and line.lstrip().startswith("- "):
            # next step starts here
            break
        end += 1

    block = lines[start:end]
    # Only inspect blocks that reference the NEAT-AI-core repository.
    block_text = "\n".join(block)
    if "stSoftwareAU/NEAT-AI-core" not in block_text:
        continue

    repo_path = None
    for line in block:
        s = line.strip()
        if s.startswith("path:"):
            value = s.split(":", 1)[1].strip().strip("'\"")
            repo_path = value
            break

    findings.append(("CHECKOUT", repo_path or ""))

# Also report whether a sibling-link step exists (detected by the step name).
for idx, line in enumerate(lines):
    if "Link NEAT-AI-core sibling path" in line:
        findings.append(("SIBLING_LINK", str(idx + 1)))
        break

for kind, value in findings:
    print(f"{kind}\t{value}")
PY
  )"

  has_checkout=0
  has_sibling_link=0
  while IFS=$'\t' read -r kind value; do
    case "$kind" in
      CHECKOUT)
        has_checkout=1
        if [[ -z "$value" ]]; then
          echo "FAIL $workflow: checkout of stSoftwareAU/NEAT-AI-core has no explicit 'path:' — must be in-workspace." >&2
          EXIT_CODE=1
        elif [[ "$value" == /* ]]; then
          echo "FAIL $workflow: checkout path '$value' is absolute — must be a relative in-workspace path." >&2
          EXIT_CODE=1
        elif [[ "$value" == ..* || "$value" == */..* ]]; then
          echo "FAIL $workflow: checkout path '$value' is outside the workspace — must be an in-workspace path." >&2
          EXIT_CODE=1
        else
          echo "OK   $workflow: NEAT-AI-core checkout path='$value'"
        fi
        ;;
      SIBLING_LINK)
        has_sibling_link=1
        ;;
    esac
  done <<< "$findings"

  if [[ "$has_checkout" -eq 1 && "$has_sibling_link" -eq 0 ]]; then
    echo "FAIL $workflow: checks out NEAT-AI-core but has no 'Link NEAT-AI-core sibling path' step — the Cargo path dependency will not resolve." >&2
    EXIT_CODE=1
  fi
done < <(find "$WORKFLOWS_DIR" -maxdepth 1 \( -name "*.yml" -o -name "*.yaml" \) -type f | sort)

if [[ "$FOUND_ANY" -eq 0 ]]; then
  echo "No workflow files found in $WORKFLOWS_DIR" >&2
  exit 2
fi

exit "$EXIT_CODE"
