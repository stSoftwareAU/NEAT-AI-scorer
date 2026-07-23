#!/usr/bin/env bats
# Tests for scripts/check-private-automation-repo-refs.sh — Issue #451.
#
# Synthetic trees in a temp directory exercise the pass/fail behaviour (exit
# codes, reported output) so the real tree is never mutated, plus one test
# asserting the shipped tree passes the guard.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-private-automation-repo-refs.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  REPO_ROOT="${BATS_TEST_DIRNAME}/../.."
  export REPO_ROOT

  TMP_DIR="$(mktemp -d)"
  export TMP_DIR

  # The private slug is assembled at run time so this test file itself never
  # contains a literal reference for the guard to trip over.
  PRIVATE_SLUG="stSoftwareAU/Vibe""Coding#1613"
  export PRIVATE_SLUG
}

teardown() {
  rm -rf "$TMP_DIR"
}

@test "passes on a tree with concept-level automation wording" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Title

`bump-deps.sh` is invoked by the automation worker before `quality.sh`.
EOF

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 0 ]
  [[ "$output" == *"free of private"* ]]
}

@test "fails when a markdown file names the private automation repo" {
  printf '# Title\n\nRefreshed per %s.\n' "$PRIVATE_SLUG" >"$TMP_DIR/README.md"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"names a private repository"* ]]
  [[ "$output" == *"README.md"* ]]
}

@test "fails when a shell comment names the private automation repo" {
  printf '#!/usr/bin/env bash\n# See %s.\n' "$PRIVATE_SLUG" >"$TMP_DIR/bump-deps.sh"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"bump-deps.sh"* ]]
}

@test "matches the repo name without an issue number" {
  printf 'Contract owned by stSoftwareAU/Vibe%s.\n' "Coding" >"$TMP_DIR/notes.md"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"notes.md"* ]]
}

@test "reports every offending file, not just the first" {
  printf 'Per %s.\n' "$PRIVATE_SLUG" >"$TMP_DIR/one.md"
  printf 'clean\n' >"$TMP_DIR/two.md"
  printf 'Also per %s.\n' "$PRIVATE_SLUG" >"$TMP_DIR/three.md"

  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
  [ "$status" -eq 1 ]
  [[ "$output" == *"one.md"* ]]
  [[ "$output" == *"three.md"* ]]
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
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
}
