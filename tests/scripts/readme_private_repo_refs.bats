#!/usr/bin/env bats
# Tests for scripts/check-readme-private-repo-refs.sh — Issue #450.
#
# Synthetic README fixtures in a temp directory exercise the pass/fail
# behaviour (exit codes, reported output) so the real README is never mutated,
# plus one test asserting the shipped README passes the guard.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-readme-private-repo-refs.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  REPO_ROOT="${BATS_TEST_DIRNAME}/../.."
  export REPO_ROOT

  TMP_DIR="$(mktemp -d)"
  export TMP_DIR
}

teardown() {
  rm -rf "$TMP_DIR"
}

@test "passes on a README with concept-level production wording" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Title

Scratch/mixed production-scale pools fall back to CPU. Production records are
9848 bytes (2461 inputs + 1 output).
EOF

  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md"
  [ "$status" -eq 0 ]
  [[ "$output" == *"free of private"* ]]
}

@test "fails when the README names the private GRQ repo" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Title

Scratch/mixed GRQ-scale pools fall back to CPU.
EOF

  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md"
  [ "$status" -eq 1 ]
  [[ "$output" == *"names a private repository"* ]]
  [[ "$output" == *"GRQ-scale"* ]]
}

@test "fails when the README names the private GRQ-cluster repo" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Title

Production GRQ-cluster records are 9848 bytes.
EOF

  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md"
  [ "$status" -eq 1 ]
  [[ "$output" == *"GRQ-cluster"* ]]
}

@test "reports every offending line, not just the first" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Title

Line one mentions GRQ.
Line two is clean.
Line three mentions GRQ-cluster.
EOF

  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md"
  [ "$status" -eq 1 ]
  [[ "$output" == *"3:Line one mentions GRQ."* ]]
  [[ "$output" == *"5:Line three mentions GRQ-cluster."* ]]
}

@test "fails loudly when the README path does not exist" {
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/missing.md"
  [ "$status" -eq 1 ]
  [[ "$output" == *"README not found"* ]]
}

@test "rejects an unknown argument with a usage error" {
  run "$SCRIPT_UNDER_TEST" --bogus
  [ "$status" -eq 2 ]
  [[ "$output" == *"Usage:"* ]]
}

@test "the shipped README passes the guard" {
  run "$SCRIPT_UNDER_TEST" --readme "$REPO_ROOT/README.md"
  [ "$status" -eq 0 ]
}
