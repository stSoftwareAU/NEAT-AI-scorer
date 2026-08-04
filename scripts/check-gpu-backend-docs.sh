#!/usr/bin/env bash
# check-gpu-backend-docs.sh — Issue #507.
#
# Guards the README's description of the `gpuBackend` JSON field against the
# labels shipped in `GpuBackendLabel::as_str` (rust_scorer/src/gpu/mod.rs).
#
# GPU kernels landed with Issues #82/#83/#182, so the field is a runtime
# diagnostic reporting the backend that *actually ran* the scoring kernel — not
# a placeholder for unfinished work. The README "Output" section must say so,
# must name every shipped backend label, and must keep its cross-link to the
# detailed "GPU mode" section.
#
# Usage:
#   check-gpu-backend-docs.sh [--readme PATH] [--source PATH]
#
# Exit codes:
#   0  the README agrees with the shipped backend labels
#   1  the README has drifted, or a file/section could not be found
#   2  usage error

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
README="$REPO_ROOT/README.md"
SOURCE="$REPO_ROOT/rust_scorer/src/gpu/mod.rs"

usage() {
  echo "Usage: $0 [--readme PATH] [--source PATH]"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --readme)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      README="$2"; shift 2 ;;
    --source)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      SOURCE="$2"; shift 2 ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      usage >&2; exit 2 ;;
  esac
done

for file in "$README" "$SOURCE"; do
  if [ ! -f "$file" ]; then
    echo "FAIL: file not found: $file" >&2
    exit 1
  fi
done

# Serialised labels come from the `Self::Variant => "label",` arms of
# GpuBackendLabel::as_str, so adding a backend fails the gate until the README
# names it.
labels=()
while IFS= read -r label; do
  labels+=("$label")
done < <(sed -n 's/^[[:space:]]*Self::[A-Za-z0-9_]* => "\([a-z0-9-]*\)",.*/\1/p' "$SOURCE")

if [ "${#labels[@]}" -eq 0 ]; then
  echo "FAIL: could not read any GpuBackendLabel::as_str labels from $SOURCE" >&2
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

output_section="$(section_of "$README" '### Output')"
if [ -z "$output_section" ]; then
  echo "FAIL: could not find the '### Output' section in $README" >&2
  exit 1
fi
output_flat="$(printf '%s\n' "$output_section" | flatten)"

fail=0

if [[ "$output_flat" != *gpuBackend* ]]; then
  echo "FAIL README 'Output' section no longer documents the gpuBackend field (Issue #507)" >&2
  exit 1
fi

# Current semantics: the field reports the backend that actually ran.
if [[ "$output_flat" == *"actually ran"* ]]; then
  echo "OK   README 'Output' section states gpuBackend reports the backend that actually ran"
else
  echo "FAIL README 'Output' section must state that gpuBackend reports the backend that actually ran the scoring kernel (Issue #507)" >&2
  fail=1
fi

for label in "${labels[@]}"; do
  if [[ "$output_flat" == *"\"$label\""* ]]; then
    echo "OK   README 'Output' section names the $label backend label"
  else
    echo "FAIL README 'Output' section does not name the \"$label\" gpuBackend label shipped in $SOURCE (Issue #507)" >&2
    fail=1
  fi
done

# The GPU mode section stays the single detailed home; keep the cross-link.
if [[ "$output_flat" == *"#gpu-mode-issues-80--83"* ]]; then
  echo "OK   README 'Output' section links to the GPU mode section"
else
  echo "FAIL README 'Output' section lost its cross-link to the GPU mode section (Issue #507)" >&2
  fail=1
fi

# Stale wording that claims GPU support has not shipped, or that the field is a
# prediction rather than a runtime diagnostic.
forbidden=(
  "until GPU kernels land"
  "would run on"
)
for phrase in "${forbidden[@]}"; do
  if [[ "$output_flat" == *"$phrase"* ]]; then
    echo "FAIL README 'Output' section revives superseded wording: '${phrase}' — GPU kernels shipped in Issues #82/#83/#182 (Issue #507)" >&2
    fail=1
  fi
done

if [ "$fail" -ne 0 ]; then
  echo "gpuBackend documentation has drifted from rust_scorer/src/gpu/mod.rs." >&2
  exit 1
fi

echo "README 'Output' section agrees with the shipped GpuBackendLabel values."
