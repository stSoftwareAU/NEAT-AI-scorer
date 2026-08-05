#!/usr/bin/env bats
# Tests for scripts/check-ci-permissions.sh — Issue #155.
#
# Exercises the least-privilege token-scope validator end-to-end with
# synthetic CI workflow YAML in temporary directories, plus one assertion
# against the real repo workflow so the enforced rules and the shipped
# workflow cannot drift apart.

load 'test_helper'

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-ci-permissions.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF_DIR="$(mktemp -d)"
  TMP_WF="$TMP_WF_DIR/ci.yml"
  export TMP_WF_DIR TMP_WF
}

teardown() {
  rm -rf "$TMP_WF_DIR"
}

# Canonical fixture: a least-privilege CI workflow. Failure tests mutate this
# one rule at a time.
write_good_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: CI
on: [pull_request]

permissions:
  contents: read

env:
  CARGO_TERM_COLOR: always

jobs:
  validation:
    name: Project Validation
    runs-on: ubuntu-latest
    steps:
      - run: echo ok

  quality:
    name: Quality Checks
    needs: [validation]
    runs-on: ubuntu-latest
    steps:
      - run: echo ok

  security:
    needs: [validation]
    permissions:
      contents: read
      checks: write
      pull-requests: write
    uses: ./.github/workflows/security.yml
    if: github.event_name == 'pull_request'
    with:
      include-dependency-review: true
EOF
}

@test "passes on a least-privilege workflow" {
  write_good_workflow "$TMP_WF"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -eq 0 ]
  # Issue #360: prove every rule was individually evaluated and passed via the
  # machine-checkable "OK   " marker rather than pinning informational wording.
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 8 ]
}

@test "fails when there is no workflow-level permissions block" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [pull_request]

env:
  CARGO_TERM_COLOR: always

jobs:
  security:
    needs: [validation]
    permissions:
      contents: read
      checks: write
      pull-requests: write
    uses: ./.github/workflows/security.yml
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no workflow-level 'permissions:' block"* ]]
}

@test "fails when the top-level default omits contents: read" {
  write_good_workflow "$TMP_WF"
  # Replace the top-level read default with an unrelated scope only.
  sed -i.bak 's/^  contents: read$/  actions: read/' "$TMP_WF"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"top-level permissions must grant 'contents: read'"* ]]
}

@test "fails when the top-level default grants a write scope" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [pull_request]

permissions:
  contents: write

jobs:
  security:
    needs: [validation]
    permissions:
      contents: read
      checks: write
      pull-requests: write
    uses: ./.github/workflows/security.yml
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"must not grant write scopes"* ]]
}

@test "fails when the security job has no permissions block" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [pull_request]

permissions:
  contents: read

jobs:
  security:
    needs: [validation]
    uses: ./.github/workflows/security.yml
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"security job must declare job-level 'permissions:'"* ]]
}

@test "fails when the security job omits checks: write" {
  write_good_workflow "$TMP_WF"
  # Drop the checks: write line from the security job's permissions block.
  grep -v 'checks: write' "$TMP_WF" >"$TMP_WF.stripped"
  mv "$TMP_WF.stripped" "$TMP_WF"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"security job must grant 'checks: write'"* ]]
}

@test "fails when the security job omits pull-requests: write" {
  write_good_workflow "$TMP_WF"
  grep -v 'pull-requests: write' "$TMP_WF" >"$TMP_WF.stripped"
  mv "$TMP_WF.stripped" "$TMP_WF"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"security job must grant 'pull-requests: write'"* ]]
}

@test "reports an error when the workflow file does not exist" {
  assert_missing_target_rejected "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF_DIR/does-not-exist.yml"
}

@test "unknown flag prints usage and exits non-zero" {
  assert_unknown_flag_rejected "$SCRIPT_UNDER_TEST"
}

@test "real repository ci.yml satisfies every least-privilege rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
