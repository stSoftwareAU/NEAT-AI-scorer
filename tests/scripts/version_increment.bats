#!/usr/bin/env bats
# Tests for scripts/version-increment.sh — guarded Cargo version bump helper.
# Exercises the script with real temporary git repositories so behaviour
# (exit codes, version strings, commit side-effects) is verified end-to-end.

load 'test_helper'

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/version-increment.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_REPO="$(mktemp -d)"
  export TMP_REPO

  (
    cd "$TMP_REPO"
    git init -q -b main
    git config user.email "test@example.com"
    git config user.name "Test User"

    mkdir -p rust_scorer
    cat >rust_scorer/Cargo.toml <<'EOF'
[package]
name = "rust_scorer"
version = "0.5.4"
edition = "2024"
EOF
    git add rust_scorer/Cargo.toml
    git commit -q -m "initial"

    git checkout -q -b feature
  )
}

teardown() {
  rm -rf "$TMP_REPO"
}

@test "get_version prints the current Cargo package version" {
  run "$SCRIPT_UNDER_TEST" --get-version --manifest "$TMP_REPO/rust_scorer/Cargo.toml"
  [ "$status" -eq 0 ]
  [ "$output" = "0.5.4" ]
}

@test "bump-patch increments only the patch component" {
  run "$SCRIPT_UNDER_TEST" --bump-patch --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --dry-run
  [ "$status" -eq 0 ]
  [[ "$output" == *"0.5.5"* ]]
}

@test "bump-patch writes the new version into the manifest" {
  run "$SCRIPT_UNDER_TEST" --bump-patch --manifest "$TMP_REPO/rust_scorer/Cargo.toml"
  [ "$status" -eq 0 ]
  run "$SCRIPT_UNDER_TEST" --get-version --manifest "$TMP_REPO/rust_scorer/Cargo.toml"
  [ "$output" = "0.5.5" ]
}

@test "already-bumped? exits 0 when branch version differs from base" {
  (
    cd "$TMP_REPO"
    "$SCRIPT_UNDER_TEST" --bump-patch --manifest rust_scorer/Cargo.toml
    git add rust_scorer/Cargo.toml
    git commit -q -m "chore: bump version"
  )
  run "$SCRIPT_UNDER_TEST" --already-bumped --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
}

@test "already-bumped? exits 1 when branch version matches base" {
  run "$SCRIPT_UNDER_TEST" --already-bumped --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO"
  [ "$status" -eq 1 ]
}

@test "run performs a bump when none has happened on the branch" {
  run "$SCRIPT_UNDER_TEST" --run --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
  [[ "$output" == *"bumped"* ]]
  run "$SCRIPT_UNDER_TEST" --get-version --manifest "$TMP_REPO/rust_scorer/Cargo.toml"
  [ "$output" = "0.5.5" ]
}

@test "run is idempotent — a second invocation on the same branch does not bump again" {
  (
    cd "$TMP_REPO"
    "$SCRIPT_UNDER_TEST" --run --manifest rust_scorer/Cargo.toml --base-ref main --repo "$TMP_REPO" >/dev/null
    git add rust_scorer/Cargo.toml
    git commit -q -m "chore: bump"
  )
  run "$SCRIPT_UNDER_TEST" --run --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
  [[ "$output" == *"skip"* ]]
  run "$SCRIPT_UNDER_TEST" --get-version --manifest "$TMP_REPO/rust_scorer/Cargo.toml"
  [ "$output" = "0.5.5" ]
}

@test "run respects a manual bump on the branch (no double-bump)" {
  (
    cd "$TMP_REPO"
    # Simulate a human manually bumping to 0.6.0
    sed -i.bak 's/version = "0.5.4"/version = "0.6.0"/' rust_scorer/Cargo.toml
    rm -f rust_scorer/Cargo.toml.bak
    git add rust_scorer/Cargo.toml
    git commit -q -m "chore: manual bump"
  )
  run "$SCRIPT_UNDER_TEST" --run --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
  [[ "$output" == *"skip"* ]]
  run "$SCRIPT_UNDER_TEST" --get-version --manifest "$TMP_REPO/rust_scorer/Cargo.toml"
  [ "$output" = "0.6.0" ]
}

@test "missing manifest yields a clear error" {
  run "$SCRIPT_UNDER_TEST" --get-version --manifest "$TMP_REPO/does-not-exist.toml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage" {
  assert_unknown_flag_rejected "$SCRIPT_UNDER_TEST"
}
