#!/usr/bin/env bash
# check-self-tuning-docs.sh — Issue #550.
#
# Guards the self-tuning reference (`docs/self-tuning.md`) against drift from
# the knob resolvers it documents:
#
#   * rust_scorer/src/host_resources.rs — worker ceiling, GPU scratch tiers,
#     nameplate tolerance, the RAM/binding share divisors;
#   * rust_scorer/src/read_tuning.rs — record-size tiers, the read-chunk RAM
#     ceiling and the aggregate read budget.
#
# Three classes of drift fail the gate:
#
#   1. Tier drift — a documented tier row whose value no longer matches the
#      constant (or match arm) that produces it.
#   2. Knob drift — a `NEAT_SCORER_*` environment read in the sources with no
#      entry in the doc's escape-hatch table (the guard's own self-check: it
#      also fails when it can find no knob at all, so a broken inventory cannot
#      pass silently).
#   3. Messaging drift — `README.md` or `docs/performance-baseline.md` losing
#      the emergency-only wording that demotes the env vars from ordinary
#      tuning knobs, or the link to the reference doc.
#
# Companion to `check-read-bytes-docs.sh` (Issue #504), which guards the
# README's own read-chunk section.
#
# Usage:
#   check-self-tuning-docs.sh [--doc PATH] [--readme PATH] [--baseline PATH]
#                             [--src DIR]
#
# Exit codes:
#   0  the documents agree with the shipped constants
#   1  a document has drifted, or a file/section could not be found
#   2  usage error

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DOC="$REPO_ROOT/docs/self-tuning.md"
README="$REPO_ROOT/README.md"
BASELINE="$REPO_ROOT/docs/performance-baseline.md"
SRC="$REPO_ROOT/rust_scorer/src"

usage() {
  echo "Usage: $0 [--doc PATH] [--readme PATH] [--baseline PATH] [--src DIR]"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --doc)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      DOC="$2"; shift 2 ;;
    --readme)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      README="$2"; shift 2 ;;
    --baseline)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      BASELINE="$2"; shift 2 ;;
    --src)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      SRC="$2"; shift 2 ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      usage >&2; exit 2 ;;
  esac
done

for file in "$DOC" "$README" "$BASELINE"; do
  if [ ! -f "$file" ]; then
    echo "FAIL: file not found: $file" >&2
    exit 1
  fi
done
if [ ! -d "$SRC" ]; then
  echo "FAIL: source directory not found: $SRC" >&2
  exit 1
fi

HOST_SRC="$SRC/host_resources.rs"
READ_SRC="$SRC/read_tuning.rs"
for file in "$HOST_SRC" "$READ_SRC"; do
  if [ ! -f "$file" ]; then
    echo "FAIL: file not found: $file" >&2
    exit 1
  fi
done

fail=0
note_fail() {
  echo "FAIL $*" >&2
  fail=1
}

GIB_BYTES=1073741824
MIB_BYTES=1048576

# --- Reading the shipped constants -----------------------------------------

# const_value <file> <NAME> — evaluate a scalar `const NAME: <int> = <expr>;`.
# Prints nothing when the constant is absent, so callers can decide whether a
# missing constant is fatal.
const_value() {
  local file="$1" name="$2" expr
  expr="$(sed -n "s/^[[:space:]]*\(pub\(([a-z]*)\)\{0,1\} \)\{0,1\}const ${name}: [a-z0-9]* = \([^;]*\);.*/\3/p" \
    "$file" | head -n 1 | tr -d '_')"
  [ -n "$expr" ] || return 0
  echo "$((expr))"
}

# require_const <file> <NAME> — `const_value`, but a missing constant is fatal:
# a renamed constant must fail the gate rather than silently drop a check.
require_const() {
  local value
  value="$(const_value "$1" "$2")"
  if [ -z "$value" ]; then
    echo "FAIL: could not read const $2 from $1" >&2
    exit 1
  fi
  echo "$value"
}

# render_bytes <bytes> — the doc-facing spelling of a byte count.
render_bytes() {
  local bytes="$1"
  if [ "$((bytes % GIB_BYTES))" -eq 0 ]; then
    echo "$((bytes / GIB_BYTES)) GiB"
  elif [ "$((bytes % MIB_BYTES))" -eq 0 ]; then
    echo "$((bytes / MIB_BYTES)) MiB"
  else
    echo "$bytes"
  fi
}

# resolve_expr <file> <expr> — evaluate a match-arm right-hand side, resolving
# `GIB`/`MIB`, any `const` defined in <file>, and the `max_read_bytes` accessor.
# Prints nothing when the expression names something it cannot resolve (a
# function call, a local binding); the caller then checks only that the row
# exists.
resolve_expr() {
  local file="$1" expr="$2" token value
  # The read ceiling is reached through its accessor, not by name.
  case "$expr" in
    *max_read_bytes*) expr="MAX_READ_BYTES" ;;
  esac
  for token in $(grep -oE '[A-Za-z_][A-Za-z0-9_]*' <<<"$expr" || true); do
    case "$token" in
      GIB) value="$GIB_BYTES" ;;
      MIB) value="$MIB_BYTES" ;;
      *) value="$(const_value "$file" "$token")" ;;
    esac
    [ -n "$value" ] || return 0
    expr="${expr//"$token"/$value}"
  done
  echo "$((${expr//_/}))"
}

# --- Reading the document ---------------------------------------------------

# section_of <file> <heading substring> — the heading's section, up to the next
# heading of the same or a higher level.
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

# require_section <file> <heading substring> — the section text, or a fatal
# error when the heading is gone.
require_section() {
  local text
  text="$(section_of "$1" "$2")"
  if [ -z "$text" ]; then
    echo "FAIL: could not find the '$2' section in $1" >&2
    exit 1
  fi
  echo "$text"
}

# row_values <section text> <label> — the second cell of every table row whose
# first cell is exactly <label> (emphasis and backticks stripped).
row_values() {
  printf '%s\n' "$1" | awk -F'|' -v want="$2" '
    function trim(s) {
      gsub(/[`*]/, "", s)
      gsub(/^[ \t]+|[ \t]+$/, "", s)
      return s
    }
    NF >= 3 && trim($2) == want { print trim($3) }
  '
}

# require_row <section text> <section name> <label> <expected value>
# Fails when no row carries the label, or when a row carrying it disagrees.
require_row() {
  local section="$1" name="$2" label="$3" expected="$4" values found=0 value
  values="$(row_values "$section" "$label")"
  while IFS= read -r value; do
    [ -n "$value" ] || continue
    found=1
    if [ "$value" = "$expected" ]; then
      echo "OK   ${name}: '${label}' -> ${expected}"
    else
      note_fail "${name}: row '${label}' documents '${value}', the code resolves '${expected}'"
    fi
  done <<<"$values"
  if [ "$found" -eq 0 ]; then
    note_fail "${name}: no '${label}' row — the code has a tier the doc does not"
  fi
}

# require_row_present <section text> <section name> <label>
require_row_present() {
  if [ -n "$(row_values "$1" "$3")" ]; then
    echo "OK   ${2}: '${3}' row present"
  else
    note_fail "${2}: no '${3}' row — the code has a tier the doc does not"
  fi
}

# Collapse whitespace and strip markdown emphasis so prose matches regardless
# of wrapping or formatting.
flatten() {
  tr '\n' ' ' | tr -d '`*' | tr -s ' '
}

# require_text <flattened text> <file label> <needle> <why>
require_text() {
  if [[ "$1" == *"$3"* ]]; then
    echo "OK   ${2} states ${4}"
  else
    note_fail "${2} is missing ${4}: '${3}'"
  fi
}

# --- 1. Tier tables ---------------------------------------------------------

# check_tier_table <source file> <arm-block start regex> <doc section heading>
#                  <renderer: count|bytes>
#
# Extracts the match arms of one tiering block and asserts the doc's table
# carries a row per arm with the value the arm resolves to. Arm labels:
# `ram < N * GIB` -> "< N GiB", `ram >= N * GIB` -> "≥ N GiB", a bare
# `None`/`_` catch-all -> "unknown", and `Some(_)` -> "≥ <largest boundary>".
check_tier_table() {
  local file="$1" start="$2" heading="$3" render="$4"
  local section arms arm label rhs boundary largest=0 rows=0 value expected

  section="$(require_section "$DOC" "$heading")"
  arms="$(awk -v start="$start" '
    !seen && $0 ~ start { seen = 1; next }
    seen && /^[[:space:]]*\};?[[:space:]]*$/ { exit }
    seen && /=>/ { print }
  ' "$file")"

  if [ -z "$arms" ]; then
    echo "FAIL: could not read the '${start}' tier arms from $file" >&2
    exit 1
  fi

  # Largest `<` boundary — the implicit floor of a `Some(_)` catch-all.
  while IFS= read -r arm; do
    boundary="$(echo "$arm" | sed -n 's/.*ram < \([0-9]*\) \* GIB.*/\1/p')"
    [ -n "$boundary" ] && [ "$boundary" -gt "$largest" ] && largest="$boundary"
  done <<<"$arms"

  while IFS= read -r arm; do
    rhs="${arm#*=>}"
    rhs="${rhs%,}"
    rhs="$(echo "$rhs" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
    label=""
    boundary="$(echo "$arm" | sed -n 's/.*ram < \([0-9]*\) \* GIB.*/\1/p')"
    if [ -n "$boundary" ]; then
      label="< ${boundary} GiB"
    elif boundary="$(echo "$arm" | sed -n 's/.*ram >= \([0-9]*\) \* GIB.*/\1/p')" \
      && [ -n "$boundary" ]; then
      label="≥ ${boundary} GiB"
    elif [[ "$arm" == *"Some(_) |"* || "$arm" == *"None =>"* || "$arm" == *"_ =>"* ]]; then
      label="unknown"
    elif [[ "$arm" == *"Some(_)"* ]]; then
      label="≥ ${largest} GiB"
    fi
    [ -n "$label" ] || continue
    rows=$((rows + 1))

    value="$(resolve_expr "$file" "$rhs")"
    if [ -z "$value" ]; then
      require_row_present "$section" "$heading" "$label"
      continue
    fi
    if [ "$render" = "bytes" ]; then
      expected="$(render_bytes "$value")"
    else
      expected="$value"
    fi
    require_row "$section" "$heading" "$label" "$expected"
  done <<<"$arms"

  if [ "$rows" -eq 0 ]; then
    echo "FAIL: no tier arms recognised in the '${start}' block of $file" >&2
    exit 1
  fi
}

check_tier_table "$HOST_SRC" "fn max_worker_count" "Worker ceiling" count
check_tier_table "$HOST_SRC" "fn ram_derived_gpu_scratch_bytes" "GPU scratch budget" bytes
check_tier_table "$READ_SRC" "let ram_cap = match" "Read-chunk RAM ceiling" bytes
check_tier_table "$READ_SRC" "let tier = match" "Aggregate read budget" bytes

# --- 2. Record-size tiers and the shared share divisors ---------------------

threshold="$(require_const "$READ_SRC" LARGE_RECORD_BYTES_THRESHOLD)"
small_default="$(render_bytes "$(require_const "$READ_SRC" DEFAULT_READ_BYTES)")"
large_default="$(render_bytes "$(require_const "$READ_SRC" LARGE_RECORD_DEFAULT_READ_BYTES)")"
max_read="$(render_bytes "$(require_const "$READ_SRC" MAX_READ_BYTES)")"

record_section="$(require_section "$DOC" "Record-size tier")"
require_row "$record_section" "Record-size tier" "< ${threshold} B" "$small_default"
require_row "$record_section" "Record-size tier" "≥ ${threshold} B" "$large_default"
record_flat="$(printf '%s\n' "$record_section" | flatten)"
require_text "$record_flat" "$DOC" "$max_read" "the read-chunk clamp (MAX_READ_BYTES)"

tolerance="$(require_const "$HOST_SRC" NAMEPLATE_TOLERANCE_DIVISOR)"
tolerance_pct="$(awk -v d="$tolerance" 'BEGIN { printf "%.2f", 100 / d }')"
unified_divisor="$(require_const "$HOST_SRC" UNIFIED_RAM_SHARE_DIVISOR)"
discrete_divisor="$(require_const "$HOST_SRC" DISCRETE_BINDING_SHARE_DIVISOR)"
read_share_divisor="$(require_const "$READ_SRC" AGGREGATE_READ_RAM_SHARE_DIVISOR)"

doc_flat="$(flatten <"$DOC")"
require_text "$doc_flat" "$DOC" "${tolerance_pct} %" "the nameplate tolerance (NAMEPLATE_TOLERANCE_DIVISOR)"
require_text "$doc_flat" "$DOC" "RAM / ${unified_divisor}" "the unified-memory scratch share"
require_text "$doc_flat" "$DOC" "limit / ${discrete_divisor}" "the discrete-adapter binding share"
require_text "$doc_flat" "$DOC" "RAM / ${read_share_divisor}" "the aggregate read-budget RAM share"

# --- 3. Knob coverage (the guard's self-check) ------------------------------

# `|| true` keeps an empty inventory reportable: without it `pipefail` would
# abort the script with no diagnostic, which is exactly the silent pass this
# self-check exists to prevent.
knobs="$( (grep -rhoE --include='*.rs' 'env::var\("NEAT_SCORER_[A-Z_]+"\)' "$SRC" \
  | grep -oE 'NEAT_SCORER_[A-Z_]+' | sort -u) || true)"
if [ -z "$knobs" ]; then
  echo "FAIL: found no NEAT_SCORER_* environment read under $SRC — the knob inventory is broken" >&2
  exit 1
fi

hatch_section="$(require_section "$DOC" "Emergency escape hatches")"
hatch_flat="$(printf '%s\n' "$hatch_section" | flatten)"
while IFS= read -r knob; do
  [ -n "$knob" ] || continue
  if [[ "$hatch_flat" == *"$knob"* ]]; then
    echo "OK   escape-hatch table documents $knob"
  else
    note_fail "$knob is read in $SRC but has no entry in the '$DOC' escape-hatch table"
  fi
done <<<"$knobs"

# --- 4. Emergency-only messaging and the cross-links -------------------------

# The demotion anchors: both phrases must survive in every document that
# presents the knobs, or the env vars read as ordinary tuning again.
readme_flat="$(flatten <"$README")"
baseline_flat="$(flatten <"$BASELINE")"
doc_name="$(basename "$DOC")"

for pair in "$DOC:$doc_flat" "$README:$readme_flat" "$BASELINE:$baseline_flat"; do
  label="${pair%%:*}"
  text="${pair#*:}"
  require_text "$text" "$label" "emergency escape hatch" "the emergency-only demotion"
  require_text "$text" "$label" "not per-host configuration" "the no-per-host-recipes rule"
done

require_text "$readme_flat" "$README" "$doc_name" "a link to the self-tuning reference"
require_text "$baseline_flat" "$BASELINE" "$doc_name" "a link to the self-tuning reference"

if [ "$fail" -ne 0 ]; then
  echo "Self-tuning documentation has drifted from the shipped knob resolvers (Issue #550)." >&2
  exit 1
fi

echo "Self-tuning docs agree with host_resources.rs and read_tuning.rs."
