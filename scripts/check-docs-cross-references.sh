#!/usr/bin/env bash
# check-docs-cross-references.sh — Issue #505.
#
# Guards against dead cross-document citations. Four documents used to cite
# sections of `AGENTS.md` that never existed ("Performance Task Workflow",
# "Human Escalation"), so an agent following the citation found nothing and
# could conclude the rule did not apply.
#
# The gate enforces three things over the live documents (README.md,
# CONTRIBUTING.md, AGENTS.md and docs/*.md):
#
#   1. CONTRIBUTING.md still defines the canonical rule sections — the single
#      home every other document points at.
#   2. Every `](target.md#anchor)` link resolves: the target file exists and the
#      anchor matches a real heading in it.
#   3. No document re-attributes those rules to AGENTS.md.
#
# `docs/archive/pr-summaries/*` — the single home for PR summaries since Issue
# #508 — holds frozen historical records of merged PRs, so it is not scanned:
# rewriting a summary would falsify the record. The `pr-summary-*` skip below
# is belt-and-braces should one ever land back in the `docs/` root.
#
# Usage:
#   check-docs-cross-references.sh [--root PATH]
#
# Exit codes:
#   0  every citation resolves and the canonical sections are present
#   1  a dead citation, a missing canonical section, or a missing file
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
  echo "FAIL: root not found: $ROOT" >&2
  exit 1
fi

CONTRIBUTING="$ROOT/CONTRIBUTING.md"
if [ ! -f "$CONTRIBUTING" ]; then
  echo "FAIL: file not found: $CONTRIBUTING" >&2
  exit 1
fi

# GitHub heading slug: drop the leading #s, unwrap links, strip emphasis and
# backticks, lowercase, drop every character that is not alphanumeric, space,
# hyphen or underscore, then replace each remaining space with a hyphen.
# Spaces are NOT collapsed — GitHub maps them one-for-one, so a heading with an
# em dash ("Hot spots — 9 May 2026") slugs to a double hyphen.
slugify() {
  printf '%s\n' "$1" \
    | sed -e 's/^#\{1,6\}[[:space:]]*//' \
          -e 's/\[\([^]]*\)\]([^)]*)/\1/g' \
          -e 's/[`*]//g' \
    | tr '[:upper:]' '[:lower:]' \
    | sed -e 's/[^a-z0-9 _-]//g' -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' \
          -e 's/ /-/g'
}

# All heading slugs of a markdown file, one per line. Fenced code blocks are
# skipped so a `# comment` inside a shell example is not mistaken for a heading.
heading_slugs() {
  local file="$1" line
  local in_fence=0
  while IFS= read -r line; do
    case "$line" in
      '```'*|'~~~'*) in_fence=$((1 - in_fence)); continue ;;
    esac
    [ "$in_fence" -eq 0 ] || continue
    case "$line" in
      '#'*) slugify "$line" ;;
    esac
  done <"$file"
}

fail=0

# --- 1. The canonical sections must exist in CONTRIBUTING.md ----------------
contributing_slugs="$(heading_slugs "$CONTRIBUTING")"
for required in performance-task-workflow human-escalation; do
  if printf '%s\n' "$contributing_slugs" | grep -qx -- "$required"; then
    echo "OK   CONTRIBUTING.md defines the canonical '#${required}' section"
  else
    echo "FAIL CONTRIBUTING.md has no '#${required}' section — it is the single home other docs cite" >&2
    fail=1
  fi
done

# --- Documents to scan ------------------------------------------------------
docs=()
for candidate in "$ROOT/README.md" "$ROOT/CONTRIBUTING.md" "$ROOT/AGENTS.md"; do
  [ -f "$candidate" ] && docs+=("$candidate")
done
if [ -d "$ROOT/docs" ]; then
  while IFS= read -r doc; do
    case "$(basename "$doc")" in
      pr-summary-*) continue ;;
    esac
    docs+=("$doc")
  done < <(find "$ROOT/docs" -maxdepth 1 -name '*.md' -type f | sort)
fi

if [ "${#docs[@]}" -eq 0 ]; then
  echo "FAIL: no documents found under $ROOT" >&2
  exit 1
fi

# --- 2. Every ](target.md#anchor) citation must resolve ---------------------
anchor_links=0
for doc in "${docs[@]}"; do
  rel="${doc#"$ROOT"/}"
  dir="$(dirname "$doc")"
  while IFS= read -r link; do
    [ -n "$link" ] || continue
    target="${link%%#*}"
    anchor="${link#*#}"
    anchor_links=$((anchor_links + 1))
    resolved="$dir/$target"
    if [ ! -f "$resolved" ]; then
      echo "FAIL ${rel}: link target does not exist: ${target}" >&2
      fail=1
      continue
    fi
    if heading_slugs "$resolved" | grep -qx -- "$anchor"; then
      echo "OK   ${rel} -> ${target}#${anchor}"
    else
      echo "FAIL ${rel}: dead anchor '#${anchor}' — ${target} has no such heading" >&2
      fail=1
    fi
  done < <(grep -oE '\]\([^)#]*\.md#[^)]+\)' "$doc" | sed -e 's/^](//' -e 's/)$//')
done

# --- 3. The rules must not be re-attributed to AGENTS.md --------------------
# AGENTS.md itself may point AT the canonical home, so only lines that cite
# AGENTS.md *as* the home (no CONTRIBUTING.md on the same line) are rejected.
for doc in "${docs[@]}"; do
  rel="${doc#"$ROOT"/}"
  while IFS= read -r offending; do
    [ -n "$offending" ] || continue
    echo "FAIL ${rel}: cites AGENTS.md as the home of a rule that lives in CONTRIBUTING.md: ${offending}" >&2
    fail=1
  done < <(grep -inE 'AGENTS\.md' "$doc" \
    | grep -iE 'performance task workflow|human escalation|before/after' \
    | grep -v 'CONTRIBUTING\.md' || true)
done

if [ "$fail" -ne 0 ]; then
  echo "Documentation cross-references are broken — see the FAIL lines above (Issue #505)." >&2
  exit 1
fi

echo "All ${anchor_links} cross-document anchor citations resolve; canonical sections present."
