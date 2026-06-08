#!/usr/bin/env bats
# Tests for scripts/check-codeowners.sh — Issue #176.
#
# Exercises the CODEOWNERS validator with synthetic CODEOWNERS fixtures in
# temporary directories so behaviour (exit codes, reported failures) is
# verified end-to-end without mutating the real CODEOWNERS file.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-codeowners.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_CO="$(mktemp -d)"
  export TMP_CO
}

teardown() {
  rm -rf "$TMP_CO"
}

# Canonical valid CODEOWNERS. Failure tests mutate this fixture to drop or
# break one rule at a time.
write_codeowners() {
  local file="$1"
  cat >"$file" <<'EOF'
# Review governance
*                       @stSoftwareAU/developers
/.github/               @stSoftwareAU/developers
/.github/workflows/     @stSoftwareAU/developers
EOF
}

@test "passes on the canonical fixture" {
  write_codeowners "$TMP_CO/CODEOWNERS"
  run "$SCRIPT_UNDER_TEST" --codeowners "$TMP_CO/CODEOWNERS"
  [ "$status" -eq 0 ]
  [[ "$output" == *"ownership rule(s)"* ]]
  [[ "$output" == *"covers .github/workflows/"* ]]
  [[ "$output" != *"FAIL"* ]]
}

@test "passes when only a catch-all rule is present" {
  printf '* @stSoftwareAU/developers\n' >"$TMP_CO/CODEOWNERS"
  run "$SCRIPT_UNDER_TEST" --codeowners "$TMP_CO/CODEOWNERS"
  [ "$status" -eq 0 ]
  [[ "$output" == *"covers .github/workflows/"* ]]
}

@test "passes when a workflow-directory rule lists an email owner" {
  printf '/.github/workflows/ maintainer@example.com\n' >"$TMP_CO/CODEOWNERS"
  run "$SCRIPT_UNDER_TEST" --codeowners "$TMP_CO/CODEOWNERS"
  [ "$status" -eq 0 ]
}

@test "fails when no rule covers the workflows directory" {
  cat >"$TMP_CO/CODEOWNERS" <<'EOF'
/docs/  @stSoftwareAU/developers
/src/   @stSoftwareAU/developers
EOF
  run "$SCRIPT_UNDER_TEST" --codeowners "$TMP_CO/CODEOWNERS"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no rule covers .github/workflows/"* ]]
}

@test "fails when a rule has a pattern but no owner" {
  printf '/.github/workflows/\n' >"$TMP_CO/CODEOWNERS"
  run "$SCRIPT_UNDER_TEST" --codeowners "$TMP_CO/CODEOWNERS"
  [ "$status" -ne 0 ]
  [[ "$output" == *"has no owner"* ]]
}

@test "fails when an owner token is malformed" {
  printf '/.github/workflows/ not-a-valid-owner\n' >"$TMP_CO/CODEOWNERS"
  run "$SCRIPT_UNDER_TEST" --codeowners "$TMP_CO/CODEOWNERS"
  [ "$status" -ne 0 ]
  [[ "$output" == *"invalid owner token"* ]]
}

@test "fails when the file has only comments" {
  cat >"$TMP_CO/CODEOWNERS" <<'EOF'
# just a comment
# another comment
EOF
  run "$SCRIPT_UNDER_TEST" --codeowners "$TMP_CO/CODEOWNERS"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no ownership rules found"* ]]
}

@test "reports an error when the CODEOWNERS file does not exist" {
  run "$SCRIPT_UNDER_TEST" --codeowners "$TMP_CO/does-not-exist"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "real repository CODEOWNERS satisfies every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
