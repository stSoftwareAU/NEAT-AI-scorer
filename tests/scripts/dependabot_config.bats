#!/usr/bin/env bats
# Tests for scripts/check-dependabot-config.sh — Issue #658.
#
# Exercises the Dependabot config validator with synthetic fixtures in
# temporary directories, so exit codes and reported failures are verified
# end-to-end without touching the real `.github/dependabot.yml`.

load 'test_helper'

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-dependabot-config.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_DEP="$(mktemp -d)"
  export TMP_DEP
  CONFIG="$TMP_DEP/dependabot.yml"
}

teardown() {
  rm -rf "$TMP_DEP"
}

# Canonical valid config. Failure tests rewrite it to break one rule at a time.
write_config() {
  cat >"$CONFIG" <<'EOF'
version: 2
updates:
  - package-ecosystem: "cargo"
    directory: "/"
    schedule:
      interval: "weekly"
    cooldown:
      default-days: 7
    ignore:
      - dependency-name: "neat-core"
EOF
}

run_check() {
  run "$SCRIPT_UNDER_TEST" --config "$CONFIG"
}

@test "passes on the canonical fixture with every rule evaluated" {
  write_config
  run_check
  [ "$status" -eq 0 ]
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 6 ]
  [[ "$output" != *"FAIL"* ]]
}

@test "real repository dependabot config satisfies every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 6 ]
}

@test "accepts a daily schedule, single quotes, comments and sibling ecosystems" {
  cat >"$CONFIG" <<'EOF'
# Dependency update channel.
version: 2
updates:
  - package-ecosystem: 'github-actions'
    directory: '/'
    schedule:
      interval: 'monthly'
  - package-ecosystem: 'cargo'  # workspace root holds Cargo.lock
    directory: '/'
    schedule:
      interval: 'daily'
    cooldown:
      default-days: 10
    ignore:
      - dependency-name: 'serde'
      - dependency-name: 'neat-core'
EOF
  run_check
  [ "$status" -eq 0 ]
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 6 ]
}

@test "accepts a directories list that includes the workspace root" {
  write_config
  sed -i 's|    directory: "/"|    directories:\n      - "/"|' "$CONFIG"
  run_check
  [ "$status" -eq 0 ]
}

@test "fails when version is not 2" {
  write_config
  sed -i 's/^version: 2/version: 1/' "$CONFIG"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"version: 2"* ]]
}

@test "fails when no cargo ecosystem is configured" {
  write_config
  sed -i 's/"cargo"/"npm"/' "$CONFIG"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"FAIL"*"cargo"* ]]
}

@test "fails when the cargo entry does not cover the workspace root" {
  write_config
  sed -i 's|directory: "/"|directory: "/rust_scorer"|' "$CONFIG"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"FAIL"*"directory"* ]]
}

@test "fails on a monthly schedule" {
  write_config
  sed -i 's/"weekly"/"monthly"/' "$CONFIG"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"FAIL"*"interval"* ]]
}

@test "fails when the cargo entry has no schedule" {
  write_config
  sed -i '/schedule:/d; /interval:/d' "$CONFIG"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"FAIL"*"interval"* ]]
}

@test "fails when the cooldown is missing" {
  write_config
  sed -i '/cooldown:/d; /default-days:/d' "$CONFIG"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"FAIL"*"cooldown"* ]]
}

@test "fails when the cooldown is shorter than seven days" {
  write_config
  sed -i 's/default-days: 7/default-days: 6/' "$CONFIG"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"FAIL"*"cooldown"* ]]
}

@test "fails when neat-core is not ignored" {
  write_config
  sed -i 's/"neat-core"/"serde"/' "$CONFIG"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"FAIL"*"neat-core"* ]]
}

@test "a neat-core ignore on a sibling ecosystem does not count for cargo" {
  write_config
  sed -i '/ignore:/d; /dependency-name/d' "$CONFIG"
  cat >>"$CONFIG" <<'EOF'
  - package-ecosystem: "github-actions"
    directory: "/"
    schedule:
      interval: "weekly"
    ignore:
      - dependency-name: "neat-core"
EOF
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"FAIL"*"neat-core"* ]]
}

@test "reports every violation rather than stopping at the first" {
  cat >"$CONFIG" <<'EOF'
version: 2
updates:
  - package-ecosystem: "cargo"
    directory: "/"
    schedule:
      interval: "monthly"
EOF
  run_check
  [ "$status" -ne 0 ]
  [ "$(grep -c '^FAIL ' <<<"$output")" -eq 3 ]
}

@test "rejects a missing config file" {
  assert_missing_target_rejected "$SCRIPT_UNDER_TEST" --config "$TMP_DEP/absent.yml"
}

@test "rejects an unknown flag" {
  assert_unknown_flag_rejected "$SCRIPT_UNDER_TEST"
}

@test "-h prints usage and exits 0" {
  run "$SCRIPT_UNDER_TEST" -h
  [ "$status" -eq 0 ]
  [[ "$output" == *"Usage"* ]]
}
