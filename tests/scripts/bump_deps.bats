#!/usr/bin/env bats
# Tests for bump-deps.sh — Cargo dependency refresh helper (Issue #55).
# Exercises the script against temporary fixtures so behaviour (exit codes,
# output, manifest mutations) is verified end-to-end.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../bump-deps.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_REPO="$(mktemp -d)"
  export TMP_REPO
  mkdir -p "$TMP_REPO/rust_scorer"
}

teardown() {
  rm -rf "$TMP_REPO"
}

write_path_manifest() {
  cat >"$TMP_REPO/rust_scorer/Cargo.toml" <<'EOF'
[package]
name = "rust_scorer"
version = "0.0.1"
edition = "2024"

[dependencies]
neat-core = { path = "../../NEAT-AI-core/neat-core" }
clap = "4"
EOF
}

write_pinned_manifest() {
  local sha="$1"
  cat >"$TMP_REPO/rust_scorer/Cargo.toml" <<EOF
[package]
name = "rust_scorer"
version = "0.0.1"
edition = "2024"

[dependencies]
neat-core = { git = "https://github.com/stSoftwareAU/NEAT-AI-core", rev = "${sha}" }
clap = "4"
EOF
}

write_pinned_section_manifest() {
  local sha="$1"
  cat >"$TMP_REPO/rust_scorer/Cargo.toml" <<EOF
[package]
name = "rust_scorer"
version = "0.0.1"
edition = "2024"

[dependencies]
clap = "4"

[dependencies.neat-core]
git = "https://github.com/stSoftwareAU/NEAT-AI-core"
rev = "${sha}"
EOF
}

@test "shows usage with --help" {
  run "$SCRIPT_UNDER_TEST" --help
  [ "$status" -eq 0 ]
  [[ "$output" == *"Usage:"* ]]
  [[ "$output" == *"--quarantine-hours"* ]]
}

@test "rejects unknown options" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"unknown option"* ]]
}

@test "rejects non-integer quarantine hours" {
  write_path_manifest
  run "$SCRIPT_UNDER_TEST" --quarantine-hours abc \
    --skip-external --skip-audit --skip-build \
    --repo "$TMP_REPO"
  [ "$status" -ne 0 ]
  [[ "$output" == *"quarantine-hours"* ]]
}

@test "internal bump: path-only neat-core dep is reported as no-op" {
  write_path_manifest
  run "$SCRIPT_UNDER_TEST" \
    --skip-external --skip-audit --skip-build \
    --repo "$TMP_REPO" \
    --neat-core-sha "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
  [ "$status" -eq 0 ]
  [[ "$output" == *"path dependency"* ]]
  [[ "$output" == *"no bumps"* ]]
}

@test "internal bump: rev pin advances when upstream Develop has moved" {
  write_pinned_manifest "1111111111111111111111111111111111111111"
  run "$SCRIPT_UNDER_TEST" \
    --skip-external --skip-audit --skip-build \
    --repo "$TMP_REPO" \
    --neat-core-sha "2222222222222222222222222222222222222222"
  [ "$status" -eq 0 ]
  [[ "$output" == *"1111111"* ]]
  [[ "$output" == *"2222222"* ]]
  grep -q '2222222222222222222222222222222222222222' "$TMP_REPO/rust_scorer/Cargo.toml"
  ! grep -q '1111111111111111111111111111111111111111' "$TMP_REPO/rust_scorer/Cargo.toml"
}

@test "internal bump: rev pin no-op when SHA already matches Develop HEAD" {
  write_pinned_manifest "abcdef0123456789abcdef0123456789abcdef01"
  run "$SCRIPT_UNDER_TEST" \
    --skip-external --skip-audit --skip-build \
    --repo "$TMP_REPO" \
    --neat-core-sha "abcdef0123456789abcdef0123456789abcdef01"
  [ "$status" -eq 0 ]
  [[ "$output" == *"already pinned"* ]]
  [[ "$output" == *"no bumps"* ]]
}

@test "internal bump: handles [dependencies.neat-core] section form" {
  write_pinned_section_manifest "1111111111111111111111111111111111111111"
  run "$SCRIPT_UNDER_TEST" \
    --skip-external --skip-audit --skip-build \
    --repo "$TMP_REPO" \
    --neat-core-sha "2222222222222222222222222222222222222222"
  [ "$status" -eq 0 ]
  [[ "$output" == *"1111111"* ]]
  [[ "$output" == *"2222222"* ]]
  grep -q '2222222222222222222222222222222222222222' "$TMP_REPO/rust_scorer/Cargo.toml"
}

@test "all skip flags: produces a clean no-op" {
  write_path_manifest
  run "$SCRIPT_UNDER_TEST" \
    --skip-internal --skip-external --skip-audit --skip-build \
    --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
  [[ "$output" == *"no bumps"* ]]
}

@test "check-published: ancient timestamp is older than quarantine (exit 0)" {
  run "$SCRIPT_UNDER_TEST" --check-published "2020-01-01T00:00:00Z" 24
  [ "$status" -eq 0 ]
}

@test "check-published: very recent timestamp is within quarantine (exit 1)" {
  # A timestamp ~1 second ago is well inside any positive quarantine window.
  recent="$(python3 -c 'import datetime; print(datetime.datetime.now(datetime.timezone.utc).isoformat())')"
  run "$SCRIPT_UNDER_TEST" --check-published "$recent" 24
  [ "$status" -eq 1 ]
}

@test "check-published: quarantine of zero hours always allows the bump" {
  recent="$(python3 -c 'import datetime; print(datetime.datetime.now(datetime.timezone.utc).isoformat())')"
  run "$SCRIPT_UNDER_TEST" --check-published "$recent" 0
  [ "$status" -eq 0 ]
}

@test "check-published: invalid timestamp surfaces an error" {
  run "$SCRIPT_UNDER_TEST" --check-published "not-a-date" 24
  [ "$status" -ne 0 ]
}
