#!/usr/bin/env bats
# Tests for scripts/check-workflow-action-versions.sh — Issue #24.
#
# Drives the validator with synthetic workflow YAML in a temp directory so
# policy behaviour (exit codes, reported failures) is verified end-to-end
# without relying on the real repo workflows staying static.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-workflow-action-versions.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# A minimal compliant workflow — every action referenced satisfies the
# current policy. Individual failure tests rewrite a single line to break
# exactly one rule at a time.
write_compliant_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Example
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: actions/cache@v5
      - uses: dtolnay/rust-toolchain@stable
      - uses: ludeeus/action-shellcheck@2.0.0
      - uses: peter-evans/create-pull-request@v8
      - uses: actions/dependency-review-action@v4
      - uses: rustsec/audit-check@v2
EOF
}

@test "passes on a workflow that satisfies every policy rule" {
  write_compliant_workflow "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"actions/checkout@v5"* ]]
  [[ "$output" == *"actions/cache@v5"* ]]
  [[ "$output" == *"peter-evans/create-pull-request@v8"* ]]
  [[ "$output" == *"ludeeus/action-shellcheck@2.0.0"* ]]
  [[ "$output" == *"Node 20 exception, tracked"* ]]
}

@test "fails when actions/checkout is older than v5" {
  write_compliant_workflow "$TMP_WF/example.yml"
  sed -i.bak 's|actions/checkout@v5|actions/checkout@v4|' "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/checkout@v4"* ]]
  [[ "$output" == *"requires @v5 or newer"* ]]
}

@test "fails when actions/cache is older than v5" {
  write_compliant_workflow "$TMP_WF/example.yml"
  sed -i.bak 's|actions/cache@v5|actions/cache@v4|' "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/cache@v4"* ]]
  [[ "$output" == *"requires @v5 or newer"* ]]
}

@test "fails when peter-evans/create-pull-request is older than v8" {
  write_compliant_workflow "$TMP_WF/example.yml"
  sed -i.bak 's|peter-evans/create-pull-request@v8|peter-evans/create-pull-request@v7|' "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"peter-evans/create-pull-request@v7"* ]]
  [[ "$output" == *"requires @v8 or newer"* ]]
}

@test "fails when ludeeus/action-shellcheck uses a branch ref" {
  write_compliant_workflow "$TMP_WF/example.yml"
  sed -i.bak 's|ludeeus/action-shellcheck@2.0.0|ludeeus/action-shellcheck@master|' "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"ludeeus/action-shellcheck@master"* ]]
  [[ "$output" == *"branch ref disallowed"* ]]
}

@test "fails when a Node 20 exception is bumped to an unknown major" {
  write_compliant_workflow "$TMP_WF/example.yml"
  # rustsec/audit-check has no v3 yet — enforcing "stay on v2" surfaces the
  # accidental bump so someone can verify the new major before we adopt it.
  sed -i.bak 's|rustsec/audit-check@v2|rustsec/audit-check@v3|' "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"rustsec/audit-check@v3"* ]]
  [[ "$output" == *"tracked Node 20 exception"* ]]
}

@test "warns but does not fail on an unknown action" {
  cat >"$TMP_WF/example.yml" <<'EOF'
name: Example
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: some-random/action@v1
EOF
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"WARN"* ]]
  [[ "$output" == *"some-random/action@v1"* ]]
  [[ "$output" == *"not in policy tables"* ]]
}

@test "ignores uses lines that live inside YAML comments" {
  cat >"$TMP_WF/example.yml" <<'EOF'
name: Example
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      # Historical note: we used to rely on `uses: actions/checkout@v4`.
      - uses: actions/checkout@v5
EOF
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" != *"actions/checkout@v4"* ]]
  [[ "$output" == *"actions/checkout@v5"* ]]
}

@test "ignores reusable-workflow calls (./.github/workflows/*.yml)" {
  cat >"$TMP_WF/example.yml" <<'EOF'
name: Example
on: [pull_request]
jobs:
  security:
    uses: ./.github/workflows/security.yml
EOF
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  # No actions to audit, but the scan still runs — exits 0 with no findings.
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
  [[ "$output" != *"WARN"* ]]
}

@test "reports an error when the workflows directory does not exist" {
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF/does-not-exist"
  [ "$status" -ne 0 ]
  [[ "$output" == *"Workflows directory not found"* ]]
}

@test "reports an error when the workflows directory is empty" {
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"No workflow files found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "real repository workflows satisfy the Node 24 compat policy" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
