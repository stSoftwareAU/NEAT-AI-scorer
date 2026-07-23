#!/usr/bin/env bash
# check-readme-private-repo-refs.sh — Issue #450.
#
# Guards the public README against naming the private `stSoftwareAU/GRQ` and
# `stSoftwareAU/GRQ-cluster` repositories. A public repo must be
# self-contained: naming a private repo points every public reader at
# something they cannot see. Production behaviour is documented at concept
# level instead ("production creature", "production-scale pools", record byte
# size), so no private repo name is needed.
#
# Usage:
#   check-readme-private-repo-refs.sh [--readme PATH]
#
# Exit codes:
#   0  no private repo name found
#   1  at least one private repo name found
#   2  usage error

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
README="$REPO_ROOT/README.md"

while [ $# -gt 0 ]; do
  case "$1" in
    --readme)
      [ $# -ge 2 ] || { echo "Usage: $0 [--readme PATH]" >&2; exit 2; }
      README="$2"
      shift 2
      ;;
    -h|--help)
      echo "Usage: $0 [--readme PATH]"
      exit 0
      ;;
    *)
      echo "Usage: $0 [--readme PATH]" >&2
      exit 2
      ;;
  esac
done

if [ ! -f "$README" ]; then
  echo "❌ README not found: $README" >&2
  exit 1
fi

# Private repo names, matched case-sensitively as whole words so ordinary
# prose is unaffected.
PRIVATE_PATTERN='\bGRQ(-cluster)?\b'

if matches="$(grep -nE "$PRIVATE_PATTERN" "$README")"; then
  echo "❌ README names a private repository (Issue #450):" >&2
  echo "$matches" >&2
  echo >&2
  echo "   Reword to concept level — describe the production creature, corpus" >&2
  echo "   record size and topology by their properties, without naming the" >&2
  echo "   private stSoftwareAU/GRQ or stSoftwareAU/GRQ-cluster repos." >&2
  exit 1
fi

echo "✅ README free of private GRQ / GRQ-cluster repo references"
