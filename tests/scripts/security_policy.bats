#!/usr/bin/env bats
# Tests for scripts/check-security-policy.sh — Issue #177.
#
# Exercises the SECURITY.md validator with synthetic policy fixtures in
# temporary directories so behaviour (exit codes, reported failures) is
# verified end-to-end without mutating the real SECURITY.md file.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-security-policy.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_SEC="$(mktemp -d)"
  export TMP_SEC
}

teardown() {
  rm -rf "$TMP_SEC"
}

# Canonical valid SECURITY.md. Failure tests mutate this fixture to drop or
# break one required element at a time.
write_policy() {
  local file="$1"
  cat >"$file" <<'EOF'
# Security Policy

## Reporting a vulnerability

Use the repository Security tab and choose **Report a vulnerability**, or
email security@example.com.

## Response targets

We acknowledge reports within 3 business days.

## Emergency dependency bump

To push an out-of-band fix, run `./bump-deps.sh --quarantine-hours 0`.

## Supported versions

| Version            | Supported          |
| ------------------ | ------------------ |
| `Develop` (latest) | :white_check_mark: |
| Older commits      | :x:                |
EOF
}

@test "passes on the canonical fixture" {
  write_policy "$TMP_SEC/SECURITY.md"
  run "$SCRIPT_UNDER_TEST" --security-policy "$TMP_SEC/SECURITY.md"
  [ "$status" -eq 0 ]
  [[ "$output" == *"private reporting route"* ]]
  [[ "$output" == *"supported-versions table"* ]]
  [[ "$output" == *"emergency dependency-bump procedure"* ]]
  [[ "$output" != *"FAIL"* ]]
}

@test "passes when only an email reporting route is given" {
  cat >"$TMP_SEC/SECURITY.md" <<'EOF'
# Security Policy

Email security@example.com to report a problem. We respond within 5 business days.

For an emergency out-of-band fix, run `./bump-deps.sh --quarantine-hours 0`.

## Supported versions

| Version | Supported |
| ------- | --------- |
| latest  | yes       |
EOF
  run "$SCRIPT_UNDER_TEST" --security-policy "$TMP_SEC/SECURITY.md"
  [ "$status" -eq 0 ]
}

@test "fails when there is no emergency dependency-bump procedure" {
  cat >"$TMP_SEC/SECURITY.md" <<'EOF'
# Security Policy

Email security@example.com to report a problem. We respond within 3 business days.

## Supported versions

| Version | Supported |
| ------- | --------- |
| latest  | yes       |
EOF
  run "$SCRIPT_UNDER_TEST" --security-policy "$TMP_SEC/SECURITY.md"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no emergency dependency-bump procedure"* ]]
}

@test "fails when the emergency procedure omits the bump tooling" {
  cat >"$TMP_SEC/SECURITY.md" <<'EOF'
# Security Policy

Email security@example.com to report a problem. We respond within 3 business days.

In an emergency, ship a fix as fast as you can.

## Supported versions

| Version | Supported |
| ------- | --------- |
| latest  | yes       |
EOF
  run "$SCRIPT_UNDER_TEST" --security-policy "$TMP_SEC/SECURITY.md"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no emergency dependency-bump procedure"* ]]
}

@test "fails when there is no private reporting route" {
  cat >"$TMP_SEC/SECURITY.md" <<'EOF'
# Security Policy

We take security seriously. We respond within 3 business days.

## Supported versions

| Version | Supported |
| ------- | --------- |
| latest  | yes       |
EOF
  run "$SCRIPT_UNDER_TEST" --security-policy "$TMP_SEC/SECURITY.md"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no private reporting route"* ]]
}

@test "fails when no response-time expectation is stated" {
  cat >"$TMP_SEC/SECURITY.md" <<'EOF'
# Security Policy

Email security@example.com to report a problem privately.

## Supported versions

| Version | Supported |
| ------- | --------- |
| latest  | yes       |
EOF
  run "$SCRIPT_UNDER_TEST" --security-policy "$TMP_SEC/SECURITY.md"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no response-time expectation"* ]]
}

@test "fails when the supported-versions table is missing" {
  cat >"$TMP_SEC/SECURITY.md" <<'EOF'
# Security Policy

Email security@example.com to report a problem. We respond within 3 business days.
EOF
  run "$SCRIPT_UNDER_TEST" --security-policy "$TMP_SEC/SECURITY.md"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no supported-versions table"* ]]
}

@test "fails when a supported-versions heading has no table" {
  cat >"$TMP_SEC/SECURITY.md" <<'EOF'
# Security Policy

Email security@example.com to report a problem. We respond within 3 business days.

## Supported versions

Only the latest commit is supported.
EOF
  run "$SCRIPT_UNDER_TEST" --security-policy "$TMP_SEC/SECURITY.md"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no supported-versions table"* ]]
}

@test "fails when the file is empty" {
  : >"$TMP_SEC/SECURITY.md"
  run "$SCRIPT_UNDER_TEST" --security-policy "$TMP_SEC/SECURITY.md"
  [ "$status" -ne 0 ]
  [[ "$output" == *"empty"* ]]
}

@test "reports an error when the policy file does not exist" {
  run "$SCRIPT_UNDER_TEST" --security-policy "$TMP_SEC/does-not-exist"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "real repository SECURITY.md satisfies every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
