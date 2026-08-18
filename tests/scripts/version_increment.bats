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

# --- downgrade refusal (Issue #567) ----------------------------------------
#
# A branch version strictly BELOW the base ref is a downgrade — the failure
# mode a bad merge conflict resolution produces. It must fail loud, never be
# mistaken for "already bumped".

# Set the branch manifest to a specific version and commit it.
set_branch_version() {
  (
    cd "$TMP_REPO"
    sed -i.bak "s/^version = .*/version = \"$1\"/" rust_scorer/Cargo.toml
    rm -f rust_scorer/Cargo.toml.bak
    git add rust_scorer/Cargo.toml
    git commit -q -m "set version $1"
  )
}

@test "already-bumped? refuses a patch downgrade against the base ref" {
  set_branch_version "0.5.3"
  run "$SCRIPT_UNDER_TEST" --already-bumped --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO"
  [ "$status" -eq 3 ]
  [[ "$output" == *"downgrade"* ]]
  [[ "$output" == *"0.5.3"* ]]
  [[ "$output" == *"0.5.4"* ]]
}

@test "already-bumped? refuses a minor downgrade against the base ref" {
  set_branch_version "0.4.9"
  run "$SCRIPT_UNDER_TEST" --already-bumped --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO"
  [ "$status" -eq 3 ]
  [[ "$output" == *"downgrade"* ]]
}

@test "already-bumped? accepts an ahead version (no re-bump forced)" {
  set_branch_version "0.6.0"
  run "$SCRIPT_UNDER_TEST" --already-bumped --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
}

@test "already-bumped? compares components numerically, not as strings" {
  # 0.10.0 is AHEAD of 0.5.4 even though it sorts before it lexically.
  set_branch_version "0.10.0"
  run "$SCRIPT_UNDER_TEST" --already-bumped --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
}

@test "already-bumped? treats a pre-release of the base version as behind it" {
  set_branch_version "0.5.4-rc1"
  run "$SCRIPT_UNDER_TEST" --already-bumped --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO"
  [ "$status" -eq 3 ]
  [[ "$output" == *"downgrade"* ]]
}

@test "run refuses a downgrade and leaves the manifest untouched" {
  set_branch_version "0.5.3"
  run "$SCRIPT_UNDER_TEST" --run --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO"
  [ "$status" -eq 3 ]
  [[ "$output" == *"downgrade"* ]]
  run "$SCRIPT_UNDER_TEST" --get-version --manifest "$TMP_REPO/rust_scorer/Cargo.toml"
  [ "$output" = "0.5.3" ]
}

@test "run accepts an ahead version without forcing another bump" {
  set_branch_version "0.10.0"
  run "$SCRIPT_UNDER_TEST" --run --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
  [[ "$output" == *"skip"* ]]
  run "$SCRIPT_UNDER_TEST" --get-version --manifest "$TMP_REPO/rust_scorer/Cargo.toml"
  [ "$output" = "0.10.0" ]
}

@test "run still bumps when the branch version equals the base" {
  run "$SCRIPT_UNDER_TEST" --run --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
  [[ "$output" == *"bumped"* ]]
  run "$SCRIPT_UNDER_TEST" --get-version --manifest "$TMP_REPO/rust_scorer/Cargo.toml"
  [ "$output" = "0.5.5" ]
}

@test "a non-semver branch version fails loud rather than being called a bump" {
  set_branch_version "not-a-version"
  run "$SCRIPT_UNDER_TEST" --already-bumped --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO"
  [ "$status" -eq 2 ]
  [[ "$output" == *"semver"* ]]
}

@test "the workflow guard/bump sequence fails CI on a downgrade" {
  # Mirrors .github/workflows/version-increment.yml: the guard runs
  # --already-bumped, and when that is non-zero the bump job runs --run.
  set_branch_version "0.5.3"
  guard_status=0
  "$SCRIPT_UNDER_TEST" --already-bumped --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO" >/dev/null 2>&1 || guard_status=$?
  [ "$guard_status" -ne 0 ]
  run "$SCRIPT_UNDER_TEST" --run --manifest "$TMP_REPO/rust_scorer/Cargo.toml" --base-ref main --repo "$TMP_REPO"
  [ "$status" -ne 0 ]
}

@test "missing manifest yields a clear error" {
  run "$SCRIPT_UNDER_TEST" --get-version --manifest "$TMP_REPO/does-not-exist.toml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage" {
  assert_unknown_flag_rejected "$SCRIPT_UNDER_TEST"
}
