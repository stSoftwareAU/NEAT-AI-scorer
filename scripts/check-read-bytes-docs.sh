#!/usr/bin/env bash
# check-read-bytes-docs.sh — Issue #504.
#
# Guards against drift between the shipped adaptive read-chunk default in
# rust_scorer/src/read_tuning.rs and the two documents that describe it:
#
#   * README.md — the "Large-record hosts" section must state the live
#     behaviour (threshold, adaptive default, cap, constants' home) and must not
#     revive the superseded "default left at 2 MiB" advice.
#   * docs/performance-baseline.md — the "Decision (Issue #307)" section records
#     dated history, so its original 2-MiB text must stay put and carry an
#     appended supersession note rather than being rewritten.
#
# Values are read from read_tuning.rs, so bumping a constant fails the gate
# until both documents follow.
#
# Usage:
#   check-read-bytes-docs.sh [--readme PATH] [--baseline PATH] [--source PATH]
#
# Exit codes:
#   0  documents agree with the shipped constants
#   1  a document has drifted, or a file/section could not be found
#   2  usage error

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
README="$REPO_ROOT/README.md"
BASELINE="$REPO_ROOT/docs/performance-baseline.md"
SOURCE="$REPO_ROOT/rust_scorer/src/read_tuning.rs"

usage() {
  echo "Usage: $0 [--readme PATH] [--baseline PATH] [--source PATH]"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --readme)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      README="$2"; shift 2 ;;
    --baseline)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      BASELINE="$2"; shift 2 ;;
    --source)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      SOURCE="$2"; shift 2 ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      usage >&2; exit 2 ;;
  esac
done

for file in "$README" "$BASELINE" "$SOURCE"; do
  if [ ! -f "$file" ]; then
    echo "FAIL: file not found: $file" >&2
    exit 1
  fi
done

# Evaluate a `const NAME: usize = <expr>;` definition (e.g. `32 * 1024 * 1024`).
const_value() {
  local name="$1" expr
  expr="$(sed -n "s/^[[:space:]]*\(pub([a-z]*) \)\{0,1\}const ${name}: usize = \([^;]*\);.*/\2/p" "$SOURCE" \
    | head -n 1 | tr -d '_')"
  if [ -z "$expr" ]; then
    echo "FAIL: could not read const ${name} from $SOURCE" >&2
    exit 1
  fi
  echo "$((expr))"
}

threshold="$(const_value LARGE_RECORD_BYTES_THRESHOLD)"
large_default="$(const_value LARGE_RECORD_DEFAULT_READ_BYTES)"
small_default="$(const_value DEFAULT_READ_BYTES)"
max_bytes="$(const_value MAX_READ_BYTES)"

large_mib=$((large_default / 1024 / 1024))
small_mib=$((small_default / 1024 / 1024))
max_mib=$((max_bytes / 1024 / 1024))

# Extract a markdown section: the heading matching $1, up to the next heading of
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

fail=0

readme_section="$(section_of "$README" 'Large-record hosts')"
if [ -z "$readme_section" ]; then
  echo "FAIL: could not find the 'Large-record hosts' section in $README" >&2
  exit 1
fi
readme_flat="$(printf '%s\n' "$readme_section" | flatten)"

require_readme() {
  local needle="$1" why="$2"
  if [[ "$readme_flat" == *"$needle"* ]]; then
    echo "OK   README documents ${why}: ${needle}"
  else
    echo "FAIL README section is missing ${why}: ${needle}" >&2
    fail=1
  fi
}

require_readme "$threshold" "the large-record threshold (LARGE_RECORD_BYTES_THRESHOLD)"
require_readme "${large_mib} MiB" "the adaptive large-record default (LARGE_RECORD_DEFAULT_READ_BYTES)"
require_readme "${small_mib} MiB" "the small-record default (DEFAULT_READ_BYTES)"
require_readme "${max_mib} MiB" "the clamp cap (MAX_READ_BYTES)"
require_readme "read_tuning" "the constants' home (read_tuning.rs)"
require_readme "NEAT_SCORER_READ_BYTES" "the env override"

# Superseded advice: the default is adaptive now, so the README must not tell
# readers it is fixed at the small-record value or that they should export the
# large-record value by hand.
forbidden=(
  "left at ${small_mib} MiB"
  "global default stays ${small_mib} MiB"
  "default is intentionally"
)
for phrase in "${forbidden[@]}"; do
  if [[ "$readme_flat" == *"$phrase"* ]]; then
    echo "FAIL README section revives superseded advice: '${phrase}' — the default is record-size adaptive" >&2
    fail=1
  fi
done

baseline_section="$(section_of "$BASELINE" 'Decision (Issue #307)')"
if [ -z "$baseline_section" ]; then
  echo "FAIL: could not find the 'Decision (Issue #307)' section in $BASELINE" >&2
  exit 1
fi
baseline_flat="$(printf '%s\n' "$baseline_section" | flatten)"

# The baseline is dated history: keep the original decision text, append the
# supersession note beneath it (mirrors the docs/gpu-scoring-design.md banner).
if [[ "$baseline_flat" == *"global default stays ${small_mib} MiB"* ]]; then
  echo "OK   performance-baseline keeps its historical ${small_mib} MiB decision text"
else
  echo "FAIL performance-baseline overwrote historical decision text; append a note instead" >&2
  fail=1
fi

if [[ "$baseline_flat" == *[Ss]uperseded* ]] && [[ "$baseline_flat" == *read_tuning* ]]; then
  echo "OK   performance-baseline decision carries a supersession note pointing at read_tuning"
else
  echo "FAIL performance-baseline decision has no supersession note naming read_tuning (superseded by the adaptive default)" >&2
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  echo "Read-chunk documentation has drifted from rust_scorer/src/read_tuning.rs." >&2
  exit 1
fi

echo "README and performance-baseline agree with the shipped read_tuning constants."
