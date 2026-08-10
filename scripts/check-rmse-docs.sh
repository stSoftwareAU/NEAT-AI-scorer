#!/usr/bin/env bash
# check-rmse-docs.sh — Issue #556.
#
# `RMSE` reuses MSE's squared-error sum and applies one host-side `sqrt` at
# finalisation (`CostKind::finalise_mean`). Because `sqrt` is monotonic the
# creature *ordering* matches `MSE`, but the *reported score* genuinely differs
# — it is in the target's own units, which is the whole reason `RMSE` exists.
# The README used to compress that into "ranks identically to MSE", which reads
# as "RMSE is redundant, remove it".
#
# The gate enforces, for the README "Cost function selector" section and the
# `CostKind::Rmse` rustdoc:
#   1. Neither claims `RMSE` ranks (creatures) identically to `MSE`.
#   2. Both state the ordering fact (same ordering as MSE).
#   3. Both state that the reported score differs, in the target's units.
#   4. The README prose says *why* the ordering holds (`sqrt` is monotonic).
#
# Usage:
#   check-rmse-docs.sh [--readme PATH] [--source PATH]
#
# Exit codes:
#   0  both documents keep ordering and reported magnitude distinct
#   1  a document has drifted back to the "identical" framing or lost a fact
#   2  usage error, or a file/section could not be found

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/check-harness.sh
source "$SCRIPT_DIR/lib/check-harness.sh"

usage() {
  cat <<'EOF'
Usage: check-rmse-docs.sh [--readme PATH] [--source PATH]

Options:
  --readme PATH  README to validate (default: README.md at the repo root).
  --source PATH  Rust source owning CostKind (default:
                 rust_scorer/src/cost.rs).
  -h, --help     Show this message.
EOF
}

README="$(check_repo_path "README.md")"
SOURCE="$(check_repo_path "rust_scorer/src/cost.rs")"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --readme)
      check_flag_value "$1" $#
      README="$2"
      shift 2
      ;;
    --source)
      check_flag_value "$1" $#
      SOURCE="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      check_unknown_arg "$1"
      ;;
  esac
done

check_require_file "$README" "README"
check_require_file "$SOURCE" "CostKind source"

# Extract a markdown section: the heading matching $2, up to the next heading of
# the same or a higher level.
section_of() {
  awk -v pat="$2" '
    /^#+ / {
      match($0, /^#+/)
      if (seen) { if (RLENGTH <= level) { exit } }
      else if (index($0, pat) > 0) { seen = 1; level = RLENGTH }
    }
    seen { print }
  ' "$1"
}

# Collapse whitespace and strip markdown emphasis/backticks so prose matches
# regardless of wrapping or formatting.
flatten() {
  tr '\n' ' ' | tr -d '`*' | tr -s ' '
}

section="$(section_of "$README" 'Cost function selector')"
if [[ -z "$section" ]]; then
  check_die 2 "could not find the 'Cost function selector' section in $README"
fi

# The `RMSE` table row is the compact claim readers meet first; the rest of the
# section is the prose that explains it.
row="$(printf '%s\n' "$section" | tr -d '`' | grep -E '^\|[[:space:]]*RMSE[[:space:]]*\|' | flatten || true)"
if [[ -z "$row" ]]; then
  check_die 2 "could not find the RMSE row of the cost table in $README"
fi
prose="$(printf '%s\n' "$section" | grep -v '^|' | flatten)"

# Doc comment attached to the `RMSE` clap variant in cost.rs.
rustdoc="$(awk '
  /^[[:space:]]*\/\/\// { buf = buf " " $0; next }
  /#\[value\(name = "RMSE"\)\]/ { print buf; exit }
  { buf = "" }
' "$SOURCE" | flatten)"
if [[ -z "$rustdoc" ]]; then
  check_die 2 "could not find the CostKind::Rmse doc comment in $SOURCE"
fi

# The framing the issue removed: "ranks identically to MSE" (and its variants)
# invites a reader to conclude RMSE is redundant.
reject_identical_claim() {
  local text="$1"
  if printf '%s' "$text" | grep -Eqi 'rank(s|ed)?( creatures)? identical'; then
    fail "claims RMSE ranks identically to MSE — say the ordering matches and the reported score differs (Issue #556)"
  else
    ok "does not claim RMSE ranks identically to MSE"
  fi
}

require_phrase() {
  local text="$1" pattern="$2" what="$3"
  if printf '%s' "$text" | grep -Eqi "$pattern"; then
    ok "states $what"
  else
    fail "does not state $what (Issue #556)"
  fi
}

check_subject "README RMSE table row"
reject_identical_claim "$row"
require_phrase "$row" 'ordering' "the ordering fact (same creature ordering as MSE)"
require_phrase "$row" "different reported score|reported score differs" "that the reported score differs from MSE's"
require_phrase "$row" 'units' "that the reported score is in the target's own units"

check_subject "README cost-selector prose"
reject_identical_claim "$prose"
require_phrase "$prose" 'ordering' "the ordering fact"
require_phrase "$prose" 'monotonic' "why the ordering holds (sqrt is monotonic)"
require_phrase "$prose" 'units' "that the reported score is in the target's own units"

check_subject "CostKind::Rmse rustdoc"
reject_identical_claim "$rustdoc"
require_phrase "$rustdoc" 'ordering' "the ordering fact"
require_phrase "$rustdoc" 'units' "that the reported score is in the target's own units"

if [[ "$EXIT_CODE" -ne 0 ]]; then
  echo "RMSE documentation conflates ordering with the reported score (Issue #556)." >&2
fi

exit "$EXIT_CODE"
