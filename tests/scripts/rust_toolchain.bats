#!/usr/bin/env bats
# Tests for scripts/check-rust-toolchain.sh — Issue #209.
#
# Exercises the pinned rust-toolchain.toml validator with synthetic fixtures in
# temporary directories so behaviour (exit codes, reported failures) is verified
# end-to-end without mutating the real rust-toolchain.toml file.

load 'test_helper'

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-rust-toolchain.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_TC="$(mktemp -d)"
  export TMP_TC
}

teardown() {
  rm -rf "$TMP_TC"
}

# Canonical pinned toolchain. Failure tests mutate this fixture to drop or
# break one rule at a time.
write_toolchain() {
  local file="$1"
  cat >"$file" <<'EOF'
[toolchain]
channel = "1.95.0"
components = ["rustfmt", "clippy"]
EOF
}

@test "passes on the canonical fixture" {
  write_toolchain "$TMP_TC/rust-toolchain.toml"
  run "$SCRIPT_UNDER_TEST" --toolchain "$TMP_TC/rust-toolchain.toml"
  [ "$status" -eq 0 ]
  # Issue #360: prove every rule was individually evaluated and passed via the
  # machine-checkable "OK   " marker rather than pinning informational wording.
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 3 ]
}

@test "fails when the [toolchain] table is missing" {
  cat >"$TMP_TC/rust-toolchain.toml" <<'EOF'
channel = "1.95.0"
components = ["rustfmt", "clippy"]
EOF
  run "$SCRIPT_UNDER_TEST" --toolchain "$TMP_TC/rust-toolchain.toml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"missing [toolchain] table"* ]]
}

@test "fails when channel floats on stable instead of a pinned version" {
  cat >"$TMP_TC/rust-toolchain.toml" <<'EOF'
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
EOF
  run "$SCRIPT_UNDER_TEST" --toolchain "$TMP_TC/rust-toolchain.toml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"concrete X.Y.Z version"* ]]
}

@test "fails when the channel key is absent" {
  cat >"$TMP_TC/rust-toolchain.toml" <<'EOF'
[toolchain]
components = ["rustfmt", "clippy"]
EOF
  run "$SCRIPT_UNDER_TEST" --toolchain "$TMP_TC/rust-toolchain.toml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not pinned"* ]]
}

@test "fails when clippy component is missing" {
  cat >"$TMP_TC/rust-toolchain.toml" <<'EOF'
[toolchain]
channel = "1.95.0"
components = ["rustfmt"]
EOF
  run "$SCRIPT_UNDER_TEST" --toolchain "$TMP_TC/rust-toolchain.toml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"both rustfmt and clippy"* ]]
}

@test "fails when the components key is absent" {
  cat >"$TMP_TC/rust-toolchain.toml" <<'EOF'
[toolchain]
channel = "1.95.0"
EOF
  run "$SCRIPT_UNDER_TEST" --toolchain "$TMP_TC/rust-toolchain.toml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"rustfmt and clippy must be declared"* ]]
}

@test "reports an error when the file does not exist" {
  assert_missing_target_rejected "$SCRIPT_UNDER_TEST" --toolchain "$TMP_TC/does-not-exist.toml"
}

@test "unknown flag prints usage and exits non-zero" {
  assert_unknown_flag_rejected "$SCRIPT_UNDER_TEST"
}

@test "real repository rust-toolchain.toml satisfies every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
