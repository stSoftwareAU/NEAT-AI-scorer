#!/usr/bin/env bats
# Tests for scripts/check-source-private-repo-refs.sh — Issue #452.
#
# Synthetic trees in a temp directory exercise the pass/fail behaviour (exit
# codes, reported output) so the real sources are never mutated, plus one test
# asserting the shipped tree passes the guard.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-source-private-repo-refs.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  REPO_ROOT="${BATS_TEST_DIRNAME}/../.."
  export REPO_ROOT

  TMP_DIR="$(mktemp -d)"
  export TMP_DIR
  mkdir -p "$TMP_DIR/rust_scorer/src" "$TMP_DIR/scripts"
}

teardown() {
  rm -rf "$TMP_DIR"
}

@test "passes on sources with concept-level production wording" {
  cat >"$TMP_DIR/rust_scorer/src/lib.rs" <<'EOF'
/// Production-scale records (~9848 bytes/record) use 32 MiB read chunks.
pub fn noop() {}
EOF

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 0 ]
  [[ "$output" == *"free of private"* ]]
}

@test "fails when a Rust comment names the private repo" {
  printf '// %s-scale pools fall back to CPU.\n' "GRQ" \
    >"$TMP_DIR/rust_scorer/src/lib.rs"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"names a private repository"* ]]
  [[ "$output" == *"rust_scorer/src/lib.rs"* ]]
}

@test "fails when a Rust test identifier or string literal names the private repo" {
  printf 'fn t() { let _ = "%s-prod"; }\n' "GRQ" \
    >"$TMP_DIR/rust_scorer/src/lib.rs"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"-prod"* ]]
}

@test "fails when AGENTS.md names the private cluster repo" {
  printf -- '- Production notes for the %s-cluster creature.\n' "GRQ" \
    >"$TMP_DIR/AGENTS.md"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"AGENTS.md"* ]]
}

@test "fails when a shell script names the private repo" {
  printf '#!/usr/bin/env bash\n# Uses the %s-cluster network.json\n' "GRQ" \
    >"$TMP_DIR/scripts/profile.sh"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"scripts/profile.sh"* ]]
}

@test "ignores historical records outside the guarded scope" {
  printf -- '- 2024 entry mentioning the %s creature.\n' "GRQ" \
    >"$TMP_DIR/CHANGELOG.md"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 0 ]
}

@test "reports every offending line, not just the first" {
  {
    printf '// line one mentions %s.\n' "GRQ"
    printf '// line two is clean.\n'
    printf '// line three mentions %s-cluster.\n' "GRQ"
  } >"$TMP_DIR/rust_scorer/src/lib.rs"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *":1:"* ]]
  [[ "$output" == *":3:"* ]]
}

@test "ordinary prose containing the letters is not matched" {
  printf '// A grquery helper and GRQX are unrelated identifiers.\n' \
    >"$TMP_DIR/rust_scorer/src/lib.rs"

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

@test "the shipped tree passes the guard" {
  run "$SCRIPT_UNDER_TEST" --root "$REPO_ROOT"
  [ "$status" -eq 0 ]
}
