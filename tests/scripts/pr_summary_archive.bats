#!/usr/bin/env bats
# Tests for scripts/check-pr-summary-archive.sh — Issue #508.
#
# Synthetic doc trees in a temp directory exercise the pass/fail behaviour
# (exit codes, reported output) so the real docs are never mutated, plus one
# test asserting the shipped tree passes the guard.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-pr-summary-archive.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  REPO_ROOT="${BATS_TEST_DIRNAME}/../.."
  export REPO_ROOT

  TMP_DIR="$(mktemp -d)"
  export TMP_DIR
  mkdir -p "$TMP_DIR/docs/archive/pr-summaries"
}

teardown() {
  rm -rf "$TMP_DIR"
}

# Builds a compliant tree: one archive, a documented convention, and a
# `.codespellrc` whose skip list follows the files into the archive.
make_valid_tree() {
  printf '# PR summary archive\n\nSummaries live here, one file per PR.\n' \
    >"$TMP_DIR/docs/archive/pr-summaries/README.md"
  printf '# Summary\n\nClosed something.\n' \
    >"$TMP_DIR/docs/archive/pr-summaries/pr-summary-42.md"
  printf '[codespell]\nskip = ./target,./docs/archive/pr-summaries/*.md\n' \
    >"$TMP_DIR/.codespellrc"
}

@test "passes on a single documented archive with a matching codespell skip" {
  make_valid_tree

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 0 ]
  [[ "$output" == *"single PR-summary archive"* ]]
}

@test "fails when a PR summary sits in the docs root instead of the archive" {
  make_valid_tree
  printf '# Summary\n\nStray.\n' >"$TMP_DIR/docs/pr-summary-7.md"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"docs/pr-summary-7.md"* ]]
  [[ "$output" == *"docs/archive/pr-summaries/"* ]]
}

@test "reports every stray summary, not just the first" {
  make_valid_tree
  printf '# Summary\n' >"$TMP_DIR/docs/pr-summary-1.md"
  printf '# Summary\n' >"$TMP_DIR/docs/pr-summary-105.md"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"docs/pr-summary-1.md"* ]]
  [[ "$output" == *"docs/pr-summary-105.md"* ]]
}

@test "fails when the codespell skip does not cover the archive" {
  make_valid_tree
  printf '[codespell]\nskip = ./target,./docs/pr-summary-*.md\n' \
    >"$TMP_DIR/.codespellrc"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"codespell"* ]]
  [[ "$output" == *"Issue #21"* ]]
}

@test "fails when the archive convention is undocumented" {
  make_valid_tree
  rm "$TMP_DIR/docs/archive/pr-summaries/README.md"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"convention"* ]]
}

@test "fails when the archive directory is missing entirely" {
  rm -rf "$TMP_DIR/docs/archive"
  printf '[codespell]\nskip = ./docs/archive/pr-summaries/*.md\n' \
    >"$TMP_DIR/.codespellrc"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"docs/archive/pr-summaries"* ]]
}

@test "fails loudly when the root does not exist" {
  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR/missing"
  [ "$status" -eq 1 ]
  [[ "$output" == *"root not found"* ]]
}

@test "rejects an unknown argument with a usage error" {
  run "$SCRIPT_UNDER_TEST" --bogus
  [ "$status" -eq 2 ]
  [[ "$output" == *"Usage:"* ]]
}

@test "the shipped docs tree passes the guard" {
  run "$SCRIPT_UNDER_TEST" --root "$REPO_ROOT"
  [ "$status" -eq 0 ]
}
