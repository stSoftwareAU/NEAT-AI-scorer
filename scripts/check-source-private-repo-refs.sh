#!/usr/bin/env bash
# check-source-private-repo-refs.sh — Issue #452.
#
# Guards the Rust sources, the shell scripts and `AGENTS.md` against naming
# the private `stSoftwareAU/GRQ` / `stSoftwareAU/GRQ-cluster` repositories.
# A public repo must be self-contained: even an incidental comment, test name
# or string literal naming a private repo points public readers at something
# they cannot see. Production behaviour is described at concept level instead
# ("production creature", "production-scale pools", "~9848 B/record"), so no
# private repo name is needed.
#
# Companion to `check-readme-private-repo-refs.sh` (Issue #450, README) and
# `check-private-automation-repo-refs.sh` (Issue #451, automation repo).
#
# Usage:
#   check-source-private-repo-refs.sh [--root PATH]
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

# Private repo names, matched case-sensitively as whole words so ordinary
# prose is unaffected.
PRIVATE_PATTERN='\bGRQ(-cluster)?\b'

# This guard — and its README-scoped sibling from Issue #450 — necessarily
# spell the names they forbid; everything else in scope must stay clean.
SELF="$(basename "${BASH_SOURCE[0]}")"
SIBLING_GUARD="scripts/check-readme-private-repo-refs.sh"

# In-scope files: Rust sources, shell scripts and the agent guidance file.
# Historical records (CHANGELOG, archived PR summaries) are deliberately out
# of scope — they document what was said at the time.
in_scope() {
  case "$1" in
    "scripts/$SELF" | "$SIBLING_GUARD") return 1 ;;
    *.rs | scripts/*.sh | AGENTS.md) return 0 ;;
    *) return 1 ;;
  esac
}

scan_matches() {
  local files=() file=""

  while IFS= read -r file; do
    in_scope "$file" && files+=("$file")
  done < <(list_candidates)

  [ ${#files[@]} -eq 0 ] && return 0
  # `|| true`: grep exits 1 when nothing matches, which is the clean case.
  # -H so a single-file match still reports its path.
  (cd "$ROOT" && grep -HInE "$PRIVATE_PATTERN" -- "${files[@]}") || true
}

# Prefer the git index (skips `target/`, build artefacts and untracked noise);
# fall back to a plain find for synthetic test trees that are not work trees.
list_candidates() {
  local top=""
  top="$(git -C "$ROOT" rev-parse --show-toplevel 2>/dev/null || true)"

  if [ -n "$top" ] && [ "$top" -ef "$ROOT" ]; then
    git -C "$ROOT" ls-files
    return
  fi

  (cd "$ROOT" && find . \
    -path ./.git -prune -o -path ./target -prune -o \
    -type f -print | sed 's|^\./||')
}

matches="$(scan_matches)"

if [ -n "$matches" ]; then
  echo "❌ Sources name a private repository (Issue #452):" >&2
  echo "$matches" >&2
  echo >&2
  echo "   Reword to concept level — describe the production creature, corpus" >&2
  echo "   record size and topology by their properties (e.g. \"production-scale\"," >&2
  echo "   \">256-neuron scratch topology\", \"~9848 B/record\") without naming" >&2
  echo "   the private repositories." >&2
  exit 1
fi

echo "✅ Rust sources, scripts and AGENTS.md free of private repo references"
