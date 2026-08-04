#!/usr/bin/env bats
# Tests for scripts/check-docs-private-repo-refs.sh — Issue #453.
#
# Synthetic doc trees in a temp directory exercise the pass/fail behaviour
# (exit codes, reported output) so the real docs are never mutated, plus one
# test asserting the shipped CHANGELOG and docs pass the guard.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-docs-private-repo-refs.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  REPO_ROOT="${BATS_TEST_DIRNAME}/../.."
  export REPO_ROOT

  TMP_DIR="$(mktemp -d)"
  mkdir -p "$TMP_DIR/docs/archive/pr-summaries"
  export TMP_DIR
}

teardown() {
  rm -rf "$TMP_DIR"
}

@test "passes on a changelog with concept-level production wording" {
  cat >"$TMP_DIR/CHANGELOG.md" <<'EOF'
# Changelog

Documented the recommended read size for the large-record production corpus.
EOF

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 0 ]
  [[ "$output" == *"free of private"* ]]
}

@test "fails when the changelog names the private GRQ repo" {
  cat >"$TMP_DIR/CHANGELOG.md" <<'EOF'
# Changelog

Documented the recommended read size for large-record GRQ corpora.
EOF

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"names a private repository"* ]]
  [[ "$output" == *"CHANGELOG.md"* ]]
}

@test "fails when an archived PR summary names the private GRQ-cluster repo" {
  cat >"$TMP_DIR/docs/archive/pr-summaries/pr-summary-99.md" <<'EOF'
# Summary

Scored against the real GRQ-cluster creature.
EOF

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"pr-summary-99.md"* ]]
  [[ "$output" == *"GRQ-cluster"* ]]
}

@test "reports every offending file, not just the first" {
  printf '# Changelog\n\nMentions GRQ.\n' >"$TMP_DIR/CHANGELOG.md"
  printf '# Summary\n\nMentions GRQ-cluster.\n' \
    >"$TMP_DIR/docs/pr-summary-7.md"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"CHANGELOG.md:3"* ]]
  [[ "$output" == *"docs/pr-summary-7.md:3"* ]]
}

@test "ignores markdown outside the changelog and docs tree" {
  printf '# Readme\n\nMentions GRQ.\n' >"$TMP_DIR/README.md"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 0 ]
}

@test "flags the remediation PR summaries too — no carve-out (Issue #510)" {
  printf '# Summary\n\nRemoved the private GRQ-cluster name.\n' \
    >"$TMP_DIR/docs/archive/pr-summaries/pr-summary-449.md"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"pr-summary-449.md"* ]]
}

@test "catches a lower-case private name inside an identifier (Issue #510)" {
  printf '# Summary\n\nRenamed `default_read_bytes_scales_for_grq_records`.\n' \
    >"$TMP_DIR/docs/archive/pr-summaries/pr-summary-98.md"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"pr-summary-98.md"* ]]
}

@test "catches a lower-case private name in a fixture filename (Issue #510)" {
  printf '# Summary\n\nRenamed the `grq-scale.json` fixture.\n' \
    >"$TMP_DIR/docs/grq-fixture-note.md"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"grq-scale.json"* ]]
}

@test "does not match unrelated words that merely contain the letters" {
  cat >"$TMP_DIR/CHANGELOG.md" <<'EOF'
# Changelog

The grqualifier field and GRQUARK constant are unrelated identifiers.
EOF

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 0 ]
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

@test "the shipped CHANGELOG and docs pass the guard" {
  run "$SCRIPT_UNDER_TEST" --root "$REPO_ROOT"
  [ "$status" -eq 0 ]
}
