#!/usr/bin/env bats
# Tests for scripts/check-rust-lints.sh — Issue #274.
#
# Exercises the crate-level rustc lint-hardening validator with synthetic
# fixtures in temporary directories so behaviour (exit codes, reported
# failures) is verified end-to-end without mutating the real manifests.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-rust-lints.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_LINTS="$(mktemp -d)"
  export TMP_LINTS
}

teardown() {
  rm -rf "$TMP_LINTS"
}

# Canonical hardened manifest. Failure tests mutate this fixture to drop or
# break one rule at a time.
write_manifest() {
  local file="$1"
  cat >"$file" <<'EOF'
[workspace]
members = ["rust_scorer"]

[workspace.lints.rust]
unsafe_op_in_unsafe_fn = "deny"
unused = "deny"

[workspace.lints.clippy]
collapsible_if = "deny"
EOF
}

# Canonical library root that scopes missing_docs to the library surface.
write_lib() {
  local file="$1"
  cat >"$file" <<'EOF'
//! Library surface.
#![warn(missing_docs)]

pub mod cost;
EOF
}

@test "passes on the canonical fixtures" {
  write_manifest "$TMP_LINTS/Cargo.toml"
  write_lib "$TMP_LINTS/lib.rs"
  run "$SCRIPT_UNDER_TEST" --manifest "$TMP_LINTS/Cargo.toml" --lib "$TMP_LINTS/lib.rs"
  [ "$status" -eq 0 ]
  [[ "$output" == *"[workspace.lints.rust] table present"* ]]
  [[ "$output" == *"unsafe_op_in_unsafe_fn denied"* ]]
  [[ "$output" == *"unused denied"* ]]
  [[ "$output" == *"missing_docs scoped to the library surface"* ]]
}

@test "fails when the [workspace.lints.rust] table is missing" {
  cat >"$TMP_LINTS/Cargo.toml" <<'EOF'
[workspace]
members = ["rust_scorer"]

[workspace.lints.clippy]
collapsible_if = "deny"
EOF
  write_lib "$TMP_LINTS/lib.rs"
  run "$SCRIPT_UNDER_TEST" --manifest "$TMP_LINTS/Cargo.toml" --lib "$TMP_LINTS/lib.rs"
  [ "$status" -ne 0 ]
  [[ "$output" == *"missing [workspace.lints.rust] table"* ]]
}

@test "fails when unsafe_op_in_unsafe_fn is not denied" {
  cat >"$TMP_LINTS/Cargo.toml" <<'EOF'
[workspace]
members = ["rust_scorer"]

[workspace.lints.rust]
unused = "deny"
EOF
  write_lib "$TMP_LINTS/lib.rs"
  run "$SCRIPT_UNDER_TEST" --manifest "$TMP_LINTS/Cargo.toml" --lib "$TMP_LINTS/lib.rs"
  [ "$status" -ne 0 ]
  [[ "$output" == *"unsafe_op_in_unsafe_fn"* ]]
}

@test "fails when unsafe_op_in_unsafe_fn is only warned, not denied" {
  cat >"$TMP_LINTS/Cargo.toml" <<'EOF'
[workspace]
members = ["rust_scorer"]

[workspace.lints.rust]
unsafe_op_in_unsafe_fn = "warn"
unused = "deny"
EOF
  write_lib "$TMP_LINTS/lib.rs"
  run "$SCRIPT_UNDER_TEST" --manifest "$TMP_LINTS/Cargo.toml" --lib "$TMP_LINTS/lib.rs"
  [ "$status" -ne 0 ]
  [[ "$output" == *"unsafe_op_in_unsafe_fn"* ]]
}

@test "fails when unused is not denied" {
  cat >"$TMP_LINTS/Cargo.toml" <<'EOF'
[workspace]
members = ["rust_scorer"]

[workspace.lints.rust]
unsafe_op_in_unsafe_fn = "deny"
EOF
  write_lib "$TMP_LINTS/lib.rs"
  run "$SCRIPT_UNDER_TEST" --manifest "$TMP_LINTS/Cargo.toml" --lib "$TMP_LINTS/lib.rs"
  [ "$status" -ne 0 ]
  [[ "$output" == *"unused"* ]]
}

@test "fails when missing_docs is not scoped in the library root" {
  write_manifest "$TMP_LINTS/Cargo.toml"
  cat >"$TMP_LINTS/lib.rs" <<'EOF'
//! Library surface.
pub mod cost;
EOF
  run "$SCRIPT_UNDER_TEST" --manifest "$TMP_LINTS/Cargo.toml" --lib "$TMP_LINTS/lib.rs"
  [ "$status" -ne 0 ]
  [[ "$output" == *"missing_docs"* ]]
}

@test "rejects a blanket deny(warnings) posture" {
  cat >"$TMP_LINTS/Cargo.toml" <<'EOF'
[workspace]
members = ["rust_scorer"]

[workspace.lints.rust]
warnings = "deny"
unsafe_op_in_unsafe_fn = "deny"
unused = "deny"
EOF
  write_lib "$TMP_LINTS/lib.rs"
  run "$SCRIPT_UNDER_TEST" --manifest "$TMP_LINTS/Cargo.toml" --lib "$TMP_LINTS/lib.rs"
  [ "$status" -ne 0 ]
  [[ "$output" == *"blanket"* ]]
}

@test "reports an error when the manifest does not exist" {
  write_lib "$TMP_LINTS/lib.rs"
  run "$SCRIPT_UNDER_TEST" --manifest "$TMP_LINTS/nope.toml" --lib "$TMP_LINTS/lib.rs"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "real repository manifest and lib satisfy every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
