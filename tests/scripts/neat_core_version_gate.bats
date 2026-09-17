#!/usr/bin/env bats
# Tests for scripts/check-neat-core-version.sh — Issues #252, #630.
#
# The gate fails CI when the PINNED neat-core release presents a breaking bump
# above scorer's recorded baseline (neat-core.expected-version), forcing a
# deliberate migration instead of following core blindly. Pre-1.0 the breaking
# component is the minor; >= 1.0 it is the major. Patch-level drift is allowed.
#
# TEST MODIFICATION (Issue #630): the pinned release is now read from
# `Cargo.lock` — the resolved `git+…?tag=v<semver>` source of the `neat-core`
# package — instead of from a sibling NEAT-AI-core checkout's
# `[workspace.package]` version, because there is no sibling checkout any more.
# The fixtures are lockfiles and the flag is `--lockfile`; every
# breaking/non-breaking decision case is unchanged.
#
# Every case drives the real script against synthetic fixtures in a temporary
# directory so behaviour (exit codes, messages) is verified end-to-end without
# touching the real baseline file or lockfile.

load 'test_helper'

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-neat-core-version.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_DIR="$(mktemp -d)"
  export TMP_DIR
  BASELINE="$TMP_DIR/neat-core.expected-version"
  LOCKFILE="$TMP_DIR/Cargo.lock"
  export BASELINE LOCKFILE
}

teardown() {
  rm -rf "$TMP_DIR"
}

# Write a baseline file containing a single version string.
write_baseline() {
  printf '%s\n' "$1" >"$BASELINE"
}

# Write a lockfile whose neat-core package is pinned to release tag v$1, in
# the shape `cargo` writes for a git-tag dependency.
write_lockfile() {
  cat >"$LOCKFILE" <<EOF
version = 4

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "neat-core"
version = "$1"
source = "git+https://github.com/stSoftwareAU/NEAT-AI-core?tag=v$1#771ad136ed106d1a13a3388e6634c451f4256f5f"
dependencies = [
 "serde",
]

[[package]]
name = "rust_scorer"
version = "1.1.80"
dependencies = [
 "neat-core",
]
EOF
}

run_gate() {
  run "$SCRIPT_UNDER_TEST" --baseline "$BASELINE" --lockfile "$LOCKFILE"
}

@test "passes when neat-core version exactly matches the baseline" {
  write_baseline "0.1.46"
  write_lockfile "0.1.46"
  run_gate
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
}

@test "passes on patch-level drift above the baseline" {
  write_baseline "0.1.40"
  write_lockfile "0.1.46"
  run_gate
  [ "$status" -eq 0 ]
  [[ "$output" == *"patch-level drift"* ]]
}

@test "fails on a pre-1.0 breaking bump (minor increased) — the #177 scenario" {
  write_baseline "0.1.46"
  write_lockfile "0.2.0"
  run_gate
  [ "$status" -eq 1 ]
  [[ "$output" == *"FAIL"* ]]
  [[ "$output" == *"breaking"* ]]
  # Failure must point at the deliberate-upgrade step.
  [[ "$output" == *"neat-core.expected-version"* ]]
}

@test "passes after the baseline acknowledges the breaking bump (#177 handled)" {
  write_baseline "0.2.0"
  write_lockfile "0.2.0"
  run_gate
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
}

@test "fails when neat-core crosses the 1.0 boundary above a 0.x baseline" {
  write_baseline "0.5.0"
  write_lockfile "1.0.0"
  run_gate
  [ "$status" -eq 1 ]
  [[ "$output" == *"breaking"* ]]
}

@test "fails on a post-1.0 major bump" {
  write_baseline "1.4.2"
  write_lockfile "2.0.0"
  run_gate
  [ "$status" -eq 1 ]
  [[ "$output" == *"breaking"* ]]
}

@test "passes on a post-1.0 minor bump (additive, non-breaking)" {
  write_baseline "1.4.2"
  write_lockfile "1.7.0"
  run_gate
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
}

@test "passes (with a note) when neat-core is behind the baseline" {
  write_baseline "0.2.0"
  write_lockfile "0.1.46"
  run_gate
  [ "$status" -eq 0 ]
  [[ "$output" == *"behind"* ]]
}

@test "ignores comments and blank lines in the baseline file" {
  cat >"$BASELINE" <<'EOF'
# Last-handled neat-core version (Issue #252).

0.1.46
EOF
  write_lockfile "0.1.46"
  run_gate
  [ "$status" -eq 0 ]
}

@test "errors when the baseline file is missing" {
  write_lockfile "0.1.46"
  run "$SCRIPT_UNDER_TEST" --baseline "$TMP_DIR/nope.version" --lockfile "$LOCKFILE"
  [ "$status" -eq 2 ]
  [[ "$output" == *"not found"* ]]
}

@test "errors when the lockfile is missing" {
  write_baseline "0.1.46"
  run "$SCRIPT_UNDER_TEST" --baseline "$BASELINE" --lockfile "$TMP_DIR/nope.lock"
  [ "$status" -eq 2 ]
  [[ "$output" == *"not found"* ]]
}

@test "errors when the baseline version is malformed" {
  write_baseline "not-a-version"
  write_lockfile "0.1.46"
  run_gate
  [ "$status" -eq 2 ]
  [[ "$output" == *"malformed"* ]]
}

@test "errors when the lockfile carries no neat-core git-tag pin" {
  write_baseline "0.1.46"
  cat >"$LOCKFILE" <<'EOF'
version = 4

[[package]]
name = "neat-core"
version = "0.1.46"
EOF
  run_gate
  [ "$status" -eq 2 ]
  [[ "$output" == *"no neat-core git-tag pin"* ]]
}

@test "reads the tag of the neat-core package, not another dependency's" {
  write_baseline "0.1.46"
  cat >"$LOCKFILE" <<'EOF'
version = 4

[[package]]
name = "other-dep"
version = "9.9.9"
source = "git+https://github.com/stSoftwareAU/NEAT-AI-other?tag=v9.9.9#abc"

[[package]]
name = "neat-core"
version = "0.1.46"
source = "git+https://github.com/stSoftwareAU/NEAT-AI-core?tag=v0.1.46#abc"
EOF
  run_gate
  [ "$status" -eq 0 ]
  [[ "$output" == *"0.1.46"* ]]
  [[ "$output" != *"9.9.9"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  assert_unknown_flag_rejected "$SCRIPT_UNDER_TEST"
}

@test "real repository baseline acknowledges the pinned neat-core release" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
