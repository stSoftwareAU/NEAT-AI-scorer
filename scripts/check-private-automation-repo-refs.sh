#!/usr/bin/env bash
# check-private-automation-repo-refs.sh — Issue #451.
#
# Guards the public tree against naming the private automation repository
# `stSoftwareAU/VibeCoding`. A public repo must be self-contained: a
# `stSoftwareAU/VibeCoding#NNNN` slug points every public reader at an issue
# they cannot open. The automation contract is described at concept level
# instead ("invoked by the automation worker before quality.sh"), so no
# private repo name is needed.
#
# Usage:
#   check-private-automation-repo-refs.sh [--root PATH]
#
# Exit codes:
#   0  no private repo name found
#   1  at least one private repo name found (or the root is missing)
#   2  usage error

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

while [ $# -gt 0 ]; do
  case "$1" in
    --root)
      [ $# -ge 2 ] || { echo "Usage: $0 [--root PATH]" >&2; exit 2; }
      ROOT="$2"
      shift 2
      ;;
    -h|--help)
      echo "Usage: $0 [--root PATH]"
      exit 0
      ;;
    *)
      echo "Usage: $0 [--root PATH]" >&2
      exit 2
      ;;
  esac
done

if [ ! -d "$ROOT" ]; then
  echo "❌ root not found: $ROOT" >&2
  exit 1
fi

# Private automation repo name, matched case-sensitively as a whole word so
# ordinary prose ("vibe coding") is unaffected.
PRIVATE_PATTERN='\bstSoftwareAU/VibeCoding\b'

# This guard and its test necessarily discuss the slug; everything else must
# stay clean.
SELF="$(basename "${BASH_SOURCE[0]}")"

if matches="$(grep -rInE "$PRIVATE_PATTERN" "$ROOT" \
  --exclude-dir=.git \
  --exclude-dir=target \
  --exclude="$SELF")"; then
  echo "❌ Tree names a private repository (Issue #451):" >&2
  echo "$matches" >&2
  echo >&2
  echo "   Reword to concept level — describe what the automation worker does" >&2
  echo "   (e.g. \"invoked by the automation worker before quality.sh\")" >&2
  echo "   without citing the private automation repo or its issue numbers." >&2
  exit 1
fi

echo "✅ Tree free of private automation repo references"
