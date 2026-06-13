#!/usr/bin/env bats
# Tests for scripts/check-rust-toolchain.sh — Issue #209.
#
# Exercises the pinned rust-toolchain.toml validator with synthetic fixtures in
# temporary directories so behaviour (exit codes, reported failures) is verified
# end-to-end without mutating the real rust-toolchain.toml file.

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
  [[ "$output" == *"[toolchain] table present"* ]]
  [[ "$output" == *"channel pinned to a concrete X.Y.Z version"* ]]
  [[ "$output" == *"rustfmt and clippy components declared"* ]]
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
  run "$SCRIPT_UNDER_TEST" --toolchain "$TMP_TC/does-not-exist.toml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "real repository rust-toolchain.toml satisfies every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
