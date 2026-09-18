#!/usr/bin/env bats
# Tests for Issue #48 — all diagrams in the repository's living docs use
# Mermaid rather than ASCII art.
#
# Historical PR summaries under docs/archive/pr-summaries/ are intentionally
# excluded: they capture the state of the repo at the time of merge and are
# not living documentation.

# Pick a UTF-8 locale the host actually provides, preferring the historical
# en_US.UTF-8. Empty output means the host has none.
pick_utf8_locale() {
  local available preferred
  available="$(locale -a 2>/dev/null || true)"
  preferred="$(printf '%s\n' "$available" | grep -ix 'en_US\.utf-\?8' | head -n 1)"
  if [ -n "$preferred" ]; then
    printf '%s\n' "$preferred"
    return 0
  fi
  printf '%s\n' "$available" | grep -iE '\.utf-?8$' | head -n 1
}

setup() {
  REPO_ROOT="${BATS_TEST_DIRNAME}/../.."
  export REPO_ROOT
  # The box-drawing grep below needs a UTF-8 locale: under C/POSIX the bracket
  # expression is matched byte-wise and hits any multi-byte character (an em
  # dash, an arrow), failing the check for reasons that have nothing to do with
  # the docs. Hardcoding en_US.UTF-8 did exactly that on unattended hosts that
  # do not ship it — bash warned and silently fell back to C (Issue #619).
  local utf8_locale
  utf8_locale="$(pick_utf8_locale)"
  if [ -z "$utf8_locale" ]; then
    echo "No UTF-8 locale available on this host — the box-drawing check cannot run reliably" >&2
    return 1
  fi
  export LC_ALL="$utf8_locale"
}

# Files we expect to be authored exclusively with Mermaid for diagrams.
# Add additional living docs to this list as they are introduced.
living_doc_paths() {
  printf '%s\n' \
    "$REPO_ROOT/README.md" \
    "$REPO_ROOT/AGENTS.md" \
    "$REPO_ROOT/docs/performance-baseline.md"
}

@test "README.md declares at least one Mermaid code block" {
  run grep -c '^```mermaid' "$REPO_ROOT/README.md"
  [ "$status" -eq 0 ]
  [ "$output" -ge 1 ]
}

@test "README.md CI job dependency graph is a Mermaid block" {
  # The "Job dependency graph" section is the only diagram in README.md
  # that previously used ASCII art (Issue #48). It must now be Mermaid.
  run awk '
    /^### Job dependency graph/ { in_section = 1; next }
    in_section && /^### / { in_section = 0 }
    in_section && /^```mermaid/ { found = 1 }
    END { exit (found ? 0 : 1) }
  ' "$REPO_ROOT/README.md"
  [ "$status" -eq 0 ]
}

@test "living docs contain no box-drawing ASCII diagrams" {
  while IFS= read -r doc; do
    [ -f "$doc" ] || continue
    if grep -n '[─│┌┐└┘├┤┬┴┼►◄▲▼]' "$doc"; then
      echo "FAIL: box-drawing characters found in $doc — convert the diagram to Mermaid"
      return 1
    fi
  done < <(living_doc_paths)
}
