#!/usr/bin/env bash
# check-docs-private-repo-refs.sh — Issue #453.
#
# Guards the historical documentation — `CHANGELOG.md` and everything under
# `docs/` (including the archived PR summaries) — against naming the private
# `stSoftwareAU/GRQ` / `stSoftwareAU/GRQ-cluster` repositories. A public repo
# must be self-contained: an archival name mention is still public
# documentation pointing readers at content they cannot see. Production
# behaviour is described at concept level instead ("production creature",
# "production-scale pools", "~9848 B/record").
#
# Companion to `check-readme-private-repo-refs.sh` (Issue #450, README),
# `check-source-private-repo-refs.sh` (Issue #452, sources/scripts/AGENTS.md)
# and `check-private-automation-repo-refs.sh` (Issue #451, automation repo).
#
# Usage:
#   check-docs-private-repo-refs.sh [--root PATH]
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

# The PR summaries that document the private-repo-reference audit itself
# necessarily quote the names they removed; everything else must stay clean.
in_scope() {
  case "$1" in
    docs/archive/pr-summaries/pr-summary-448.md \
      | docs/archive/pr-summaries/pr-summary-449.md \
      | docs/archive/pr-summaries/pr-summary-450.md) return 1 ;;
    # `*` spans `/` in a case pattern, so this covers docs/ at any depth.
    CHANGELOG.md | docs/*.md) return 0 ;;
    *) return 1 ;;
  esac
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

matches="$(scan_matches)"

if [ -n "$matches" ]; then
  echo "❌ Documentation names a private repository (Issue #453):" >&2
  echo "$matches" >&2
  echo >&2
  echo "   Reword to concept level — describe the production creature, corpus" >&2
  echo "   record size and topology by their properties (e.g. \"production-scale\"," >&2
  echo "   \">256-neuron scratch topology\", \"~9848 B/record\") without naming" >&2
  echo "   the private repositories." >&2
  exit 1
fi

echo "✅ CHANGELOG and docs free of private repo references"
