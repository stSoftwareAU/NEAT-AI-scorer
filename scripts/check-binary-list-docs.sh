#!/usr/bin/env bash
# check-binary-list-docs.sh — Issue #509.
#
# Keeps the workspace binary list single-homed. `rust_scorer/Cargo.toml` owns
# the `[[bin]]` targets; the README "Binaries" section is the one place that
# writes them out in prose. CONTRIBUTING.md and AGENTS.md each used to carry
# their own partial copy, so every new binary re-opened the same drift (they
# named three and two binaries respectively while the manifest declared four).
#
# The gate enforces:
#   1. The README "Binaries" section names every `[[bin]]` target.
#   2. CONTRIBUTING.md and AGENTS.md name no binary other than the workspace
#      member `rust_scorer` — no private copy of the list to go stale.
#   3. Both cite the home of the list (the README Binaries section or the
#      manifest), so a reader is never left without it.
#
# Usage:
#   check-binary-list-docs.sh [--manifest PATH] [--readme PATH]
#                             [--contributing PATH] [--agents PATH]
#
# Exit codes:
#   0  the binary list is single-homed and the README is in step with the manifest
#   1  a doc has drifted, lost its citation, or a file/section could not be found
#   2  usage error

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
MANIFEST="$REPO_ROOT/rust_scorer/Cargo.toml"
README="$REPO_ROOT/README.md"
CONTRIBUTING="$REPO_ROOT/CONTRIBUTING.md"
AGENTS="$REPO_ROOT/AGENTS.md"

usage() {
  echo "Usage: $0 [--manifest PATH] [--readme PATH] [--contributing PATH] [--agents PATH]"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --manifest)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      MANIFEST="$2"; shift 2 ;;
    --readme)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      README="$2"; shift 2 ;;
    --contributing)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      CONTRIBUTING="$2"; shift 2 ;;
    --agents)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      AGENTS="$2"; shift 2 ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      usage >&2; exit 2 ;;
  esac
done

for file in "$MANIFEST" "$README" "$CONTRIBUTING" "$AGENTS"; do
  if [ ! -f "$file" ]; then
    echo "FAIL: file not found: $file" >&2
    exit 1
  fi
done

# `name = "..."` lines that follow a `[[bin]]` header, so adding a binary fails
# the gate until the README names it.
bins=()
while IFS= read -r bin; do
  bins+=("$bin")
done < <(awk '
  /^[[:space:]]*\[\[bin\]\]/ { in_bin = 1; next }
  /^[[:space:]]*\[/          { in_bin = 0 }
  in_bin && /^[[:space:]]*name[[:space:]]*=/ {
    if (match($0, /"[^"]+"/)) {
      print substr($0, RSTART + 1, RLENGTH - 2)
      in_bin = 0
    }
  }
' "$MANIFEST")

if [ "${#bins[@]}" -eq 0 ]; then
  echo "FAIL: could not read any [[bin]] targets from $MANIFEST" >&2
  exit 1
fi

# Extract a markdown section: the heading matching $2, up to the next heading of
# the same or a higher level.
section_of() {
  local file="$1" pattern="$2"
  awk -v pat="$pattern" '
    /^#+ / {
      match($0, /^#+/)
      if (seen) { if (RLENGTH <= level) { exit } }
      else if (index($0, pat) > 0) { seen = 1; level = RLENGTH }
    }
    seen { print }
  ' "$file"
}

# Collapse whitespace and strip markdown emphasis/backticks so prose matches
# regardless of wrapping or formatting.
flatten() {
  tr '\n' ' ' | tr -d '`*' | tr -s ' '
}

binaries_section="$(section_of "$README" '### Binaries')"
if [ -z "$binaries_section" ]; then
  echo "FAIL: could not find the '### Binaries' section in $README" >&2
  exit 1
fi
binaries_flat="$(printf '%s\n' "$binaries_section" | flatten)"

fail=0

# --- 1. The README section names every shipped binary -----------------------
for bin in "${bins[@]}"; do
  if [[ "$binaries_flat" == *"$bin"* ]]; then
    echo "OK   README 'Binaries' section names $bin"
  else
    echo "FAIL README 'Binaries' section does not name the $bin binary declared in $MANIFEST (Issue #509)" >&2
    fail=1
  fi
done

# --- 2 & 3. The satellite docs cite the list; they do not restate it --------
# `rust_scorer` is the workspace member's own name, so naming it is expected —
# every other binary in the manifest is a private copy of the list.
check_satellite() {
  local file="$1" label="$2" flat bin
  flat="$(flatten <"$file")"

  for bin in "${bins[@]}"; do
    [ "$bin" = "rust_scorer" ] && continue
    if [[ "$flat" == *"$bin"* ]]; then
      echo "FAIL ${label} restates the binary list — it names $bin, which belongs only in ${MANIFEST##*/} and the README 'Binaries' section (Issue #509)" >&2
      fail=1
    fi
  done

  if [[ "$flat" == *"README.md#binaries"* ]] || [[ "$flat" == *"rust_scorer/Cargo.toml"* ]]; then
    echo "OK   ${label} cites the binary list home instead of copying it"
  else
    echo "FAIL ${label} must cite the binary list home — link the README 'Binaries' section or rust_scorer/Cargo.toml (Issue #509)" >&2
    fail=1
  fi
}

check_satellite "$CONTRIBUTING" "CONTRIBUTING.md"
check_satellite "$AGENTS" "AGENTS.md"

if [ "$fail" -ne 0 ]; then
  echo "The workspace binary list has drifted across the docs (Issue #509)." >&2
  exit 1
fi

echo "Binary list is single-homed: README 'Binaries' matches ${MANIFEST##*/}; CONTRIBUTING.md and AGENTS.md cite it."
