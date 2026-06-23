#!/usr/bin/env bats
# Tests for scripts/check-neat-core-version.sh — Issue #252.
#
# The gate fails CI when the sibling neat-core presents a breaking bump above
# scorer's recorded baseline (neat-core.expected-version), forcing a deliberate
# upgrade instead of tracking head blindly. Pre-1.0 the breaking component is
# the minor; >= 1.0 it is the major. Patch-level drift is allowed.
#
# Every case drives the real script against synthetic fixtures in a temporary
# directory so behaviour (exit codes, messages) is verified end-to-end without
# touching the real baseline file or the sibling clone.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-neat-core-version.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_DIR="$(mktemp -d)"
  export TMP_DIR
  BASELINE="$TMP_DIR/neat-core.expected-version"
  CORE_MANIFEST="$TMP_DIR/Cargo.toml"
  export BASELINE CORE_MANIFEST
}

teardown() {
  rm -rf "$TMP_DIR"
}

# Write a baseline file containing a single version string.
write_baseline() {
  printf '%s\n' "$1" >"$BASELINE"
}

# Write a neat-core workspace manifest carrying [workspace.package] version.
write_core_manifest() {
  cat >"$CORE_MANIFEST" <<EOF
[workspace]
members = ["neat-core"]
resolver = "2"

[workspace.package]
version = "$1"
edition = "2024"
EOF
}

run_gate() {
  run "$SCRIPT_UNDER_TEST" --baseline "$BASELINE" --core-manifest "$CORE_MANIFEST"
}

@test "passes when neat-core version exactly matches the baseline" {
  write_baseline "0.1.46"
  write_core_manifest "0.1.46"
  run_gate
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
}

@test "passes on patch-level drift above the baseline" {
  write_baseline "0.1.40"
  write_core_manifest "0.1.46"
  run_gate
  [ "$status" -eq 0 ]
  [[ "$output" == *"patch-level drift"* ]]
}

@test "fails on a pre-1.0 breaking bump (minor increased) — the #177 scenario" {
  write_baseline "0.1.46"
  write_core_manifest "0.2.0"
  run_gate
  [ "$status" -eq 1 ]
  [[ "$output" == *"FAIL"* ]]
  [[ "$output" == *"breaking"* ]]
  # Failure must point at the deliberate-upgrade step.
  [[ "$output" == *"neat-core.expected-version"* ]]
}

@test "passes after the baseline acknowledges the breaking bump (#177 handled)" {
  write_baseline "0.2.0"
  write_core_manifest "0.2.0"
  run_gate
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
}

@test "fails when neat-core crosses the 1.0 boundary above a 0.x baseline" {
  write_baseline "0.5.0"
  write_core_manifest "1.0.0"
  run_gate
  [ "$status" -eq 1 ]
  [[ "$output" == *"breaking"* ]]
}

@test "fails on a post-1.0 major bump" {
  write_baseline "1.4.2"
  write_core_manifest "2.0.0"
  run_gate
  [ "$status" -eq 1 ]
  [[ "$output" == *"breaking"* ]]
}

@test "passes on a post-1.0 minor bump (additive, non-breaking)" {
  write_baseline "1.4.2"
  write_core_manifest "1.7.0"
  run_gate
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
}

@test "passes (with a note) when neat-core is behind the baseline" {
  write_baseline "0.2.0"
  write_core_manifest "0.1.46"
  run_gate
  [ "$status" -eq 0 ]
  [[ "$output" == *"behind"* ]]
}

@test "ignores comments and blank lines in the baseline file" {
  cat >"$BASELINE" <<'EOF'
# Last-handled neat-core version (Issue #252).

0.1.46
EOF
  write_core_manifest "0.1.46"
  run_gate
  [ "$status" -eq 0 ]
}

@test "errors when the baseline file is missing" {
  write_core_manifest "0.1.46"
  run "$SCRIPT_UNDER_TEST" --baseline "$TMP_DIR/nope.version" --core-manifest "$CORE_MANIFEST"
  [ "$status" -eq 2 ]
  [[ "$output" == *"not found"* ]]
}

@test "errors when the neat-core manifest is missing" {
  write_baseline "0.1.46"
  run "$SCRIPT_UNDER_TEST" --baseline "$BASELINE" --core-manifest "$TMP_DIR/nope.toml"
  [ "$status" -eq 2 ]
  [[ "$output" == *"not found"* ]]
}

@test "errors when the baseline version is malformed" {
  write_baseline "not-a-version"
  write_core_manifest "0.1.46"
  run_gate
  [ "$status" -eq 2 ]
  [[ "$output" == *"malformed"* ]]
}

@test "errors when the manifest has no [workspace.package] version" {
  write_baseline "0.1.46"
  cat >"$CORE_MANIFEST" <<'EOF'
[workspace]
members = ["neat-core"]
EOF
  run_gate
  [ "$status" -eq 2 ]
  [[ "$output" == *"version"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "real repository baseline matches the sibling neat-core when present" {
  REPO_ROOT="${BATS_TEST_DIRNAME}/../.."
  SIBLING_MANIFEST="$REPO_ROOT/../NEAT-AI-core/Cargo.toml"
  if [ ! -f "$SIBLING_MANIFEST" ]; then
    skip "sibling NEAT-AI-core clone not present"
  fi
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
