#!/usr/bin/env bats
# Tests for Issue #48 — all diagrams in the repository's living docs use
# Mermaid rather than ASCII art.
#
# Historical PR summaries under docs/pr-summary-*.md are intentionally
# excluded: they capture the state of the repo at the time of merge and are
# not living documentation.

setup() {
  REPO_ROOT="${BATS_TEST_DIRNAME}/../.."
  export REPO_ROOT
  export LC_ALL="${LC_ALL:-en_US.UTF-8}"
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
