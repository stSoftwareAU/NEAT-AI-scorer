#!/usr/bin/env bash
# Guard: risk-bearing multi-line `run:` blocks must open with `set -euo pipefail`
# (Issue #400).
#
# A multi-line `run: |` block without `set -euo pipefail` lets a failing
# intermediate command be swallowed: the step reports success while doing less
# than it claims. For gating jobs (e.g. `shell-checks`) that turns a broken lint
# invocation into a false green on a required check — the exact
# "never fail silently" hazard the house style guards against.
#
# The repository already opens its multi-line scripts with `set -euo pipefail`
# (auto-format.yml, cargo-quality.yml, gitleaks.yml, version-increment.yml). This
# guard asserts that convention for the *risk-bearing* block shapes the audit
# flagged, so a copy-pasted block cannot drift back to the unsafe form:
#
#   * the NEAT-AI-core sibling symlink block (`ln -s … NEAT-AI-core`) — five
#     copies across ci.yml, security.yml and sbom.yml;
#   * shell-script discovery driven by `find … -name "*.sh"` (bash-syntax and
#     ShellCheck steps) — a failing `find` must not yield an empty loop and a
#     green step;
#   * privileged deletion (`sudo rm -rf`) — the "Free up runner disk space"
#     step.
#
# The script takes an optional `--workflows DIR` argument so BATS tests can
# exercise it against fixtures. With no argument it scans `.github/workflows`
# relative to the repo root.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-run-block-safety.sh [--workflows DIR]

Options:
  --workflows DIR   Directory containing workflow YAML files (default:
                    .github/workflows relative to the repo root).
  -h, --help        Show this message.

Exits 0 when every risk-bearing multi-line `run:` block (NEAT-AI-core symlink,
find-driven shell discovery, or `sudo rm -rf`) opens with `set -euo pipefail`.
Exits 1 (listing each offender as `file:line`) when any such block does not.
EOF
}

WORKFLOWS_DIR=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --workflows)
      WORKFLOWS_DIR="${2:-}"
      [[ -n "$WORKFLOWS_DIR" ]] || { echo "--workflows requires a directory argument" >&2; exit 2; }
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

# Collect offenders as "file:line" strings. A block is examined only when it is
# a `run: |` (literal block scalar); folded/inline `run:` forms are ignored.
offenders=()

scan_workflow() {
  local file="$1"
  awk '
    # Detect the start of a literal block scalar `run: |`.
    /^[[:space:]]*run:[[:space:]]*\|[[:space:]]*$/ {
      # Indentation of the `run:` key; block body is more indented than this.
      match($0, /^[[:space:]]*/)
      key_indent = RLENGTH
      start_line = NR
      first_stmt = ""
      body = ""
      # Consume the block body: lines indented deeper than the key, or blank.
      # A dedent (indent <= key) ends the block. The dedented line cannot begin
      # a deeper `run:` block, so it is safe not to re-evaluate it.
      while ((getline line) > 0) {
        if (line ~ /^[[:space:]]*$/) { body = body "\n" line; continue }
        match(line, /^[[:space:]]*/)
        if (RLENGTH <= key_indent) break
        if (first_stmt == "") { first_stmt = line }
        body = body "\n" line
      }
      # Is this a risk-bearing block? (symlink / find-*.sh / sudo rm -rf)
      risky = 0
      if (body ~ /ln[[:space:]]+-s/) risky = 1
      if (body ~ /find[^\n]*-name[[:space:]]*"\*\.sh"/) risky = 1
      if (body ~ /sudo[[:space:]]+rm[[:space:]]+-rf/) risky = 1
      if (risky && first_stmt !~ /^[[:space:]]*set[[:space:]]+-euo[[:space:]]+pipefail[[:space:]]*$/) {
        printf "%s:%d\n", FILENAME, start_line
      }
      next
    }
  ' "$file"
}

while IFS= read -r workflow; do
  [[ -f "$workflow" ]] || continue
  while IFS= read -r hit; do
    [[ -n "$hit" ]] && offenders+=("$hit")
  done < <(scan_workflow "$workflow")
done < <(find "$WORKFLOWS_DIR" -maxdepth 1 \( -name "*.yml" -o -name "*.yaml" \) -type f | sort)

if [[ ${#offenders[@]} -eq 0 ]]; then
  echo "OK   every risk-bearing multi-line run: block opens with 'set -euo pipefail'"
  exit 0
fi

echo "FAIL risk-bearing multi-line run: block missing 'set -euo pipefail' (Issue #400):" >&2
printf '  - %s\n' "${offenders[@]}" >&2
exit 1
