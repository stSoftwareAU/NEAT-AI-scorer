#!/usr/bin/env bash
# check-pr-summary-archive.sh — Issue #508.
#
# The PR-summary archive is the project's durable cross-machine memory. It used
# to be split across two homes — `docs/pr-summary-*.md` and
# `docs/archive/pr-summaries/` — with no documented convention, so an agent
# mining prior learnings could sweep one location and miss the other.
#
# This gate keeps the archive single and self-describing:
#
#   1. No PR summary sits in the `docs/` root — they all live under
#      `docs/archive/pr-summaries/`.
#   2. The `.codespellrc` skip list covers the archive path, so the Issue #21
#      typo-fixture exemption follows the files.
#   3. The convention is documented in `docs/archive/pr-summaries/README.md`.
#
# Usage:
#   check-pr-summary-archive.sh [--root PATH]
#
# Exit codes:
#   0  one documented archive, covered by the codespell skip
#   1  a stray summary, an uncovered skip list, or a missing convention doc
#   2  usage error

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

usage() {
  echo "Usage: $0 [--root PATH]"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --root)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      ROOT="$2"; shift 2 ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      usage >&2; exit 2 ;;
  esac
done

if [ ! -d "$ROOT" ]; then
  echo "❌ root not found: $ROOT" >&2
  exit 1
fi

ARCHIVE_REL="docs/archive/pr-summaries"
ARCHIVE="$ROOT/$ARCHIVE_REL"
fail=0

# --- 1. The archive is the single home --------------------------------------
if [ ! -d "$ARCHIVE" ]; then
  echo "❌ missing PR-summary archive: $ARCHIVE_REL (Issue #508)" >&2
  fail=1
fi

strays="$(find "$ROOT/docs" -maxdepth 1 -name 'pr-summary-*.md' -type f 2>/dev/null | sort || true)"
if [ -n "$strays" ]; then
  echo "❌ PR summaries found outside the archive (Issue #508):" >&2
  while IFS= read -r stray; do
    echo "   ${stray#"$ROOT"/}" >&2
  done <<<"$strays"
  echo >&2
  echo "   Move them into ${ARCHIVE_REL}/ — one archive, one file per PR." >&2
  fail=1
fi

# --- 2. The codespell skip follows the files --------------------------------
CODESPELLRC="$ROOT/.codespellrc"
if [ ! -f "$CODESPELLRC" ]; then
  echo "❌ .codespellrc not found: $CODESPELLRC" >&2
  fail=1
elif ! grep -qE "^skip[[:space:]]*=.*\./${ARCHIVE_REL}/\*\.md" "$CODESPELLRC"; then
  echo "❌ .codespellrc skip list does not cover ./${ARCHIVE_REL}/*.md (Issue #508)" >&2
  echo "   The typo-fixture exemption (Issue #21) must follow the summaries" >&2
  echo "   into the archive, or codespell fails on quoted fixtures." >&2
  fail=1
fi

# --- 3. The convention is written down --------------------------------------
CONVENTION="$ARCHIVE/README.md"
if [ ! -f "$CONVENTION" ]; then
  echo "❌ the archive convention is undocumented (Issue #508):" >&2
  echo "   expected ${ARCHIVE_REL}/README.md stating that PR summaries live" >&2
  echo "   under ${ARCHIVE_REL}/, one file per PR." >&2
  fail=1
fi

[ "$fail" -eq 0 ] || exit 1

echo "✅ single PR-summary archive at ${ARCHIVE_REL}/, documented and codespell-skipped"
