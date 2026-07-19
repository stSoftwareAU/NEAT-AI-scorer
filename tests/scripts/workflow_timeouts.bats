#!/usr/bin/env bats
# Tests for scripts/check-workflow-timeouts.sh — Issue #154.
#
# Exercises the per-job `timeout-minutes` validator end-to-end with synthetic
# workflow YAML in temporary directories, plus one assertion against the real
# repo workflows so the enforced rule and the shipped workflows cannot drift
# apart.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-workflow-timeouts.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF_DIR="$(mktemp -d)"
  TMP_WF="$TMP_WF_DIR/wf.yml"
  export TMP_WF_DIR TMP_WF
}

teardown() {
  rm -rf "$TMP_WF_DIR"
}

# Canonical fixture: a workflow where every normal job declares a sane
# timeout and the reusable-call job correctly declares none.
write_good_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: CI
on: [pull_request]

jobs:
  quality:
    name: Quality Checks
    runs-on: ubuntu-latest
    timeout-minutes: 30
    steps:
      - run: echo ok

  lint:
    name: Lint
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - run: echo ok

  security:
    needs: [quality]
    uses: ./.github/workflows/security.yml
    if: github.event_name == 'pull_request'
EOF
}

@test "passes when every normal job declares a sane timeout" {
  write_good_workflow "$TMP_WF"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -eq 0 ]
  # Issue #360: prove every rule was individually evaluated and passed via the
  # machine-checkable "OK   " marker rather than pinning informational wording.
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 3 ]
}

@test "fails when a normal job has no timeout-minutes" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [push]

jobs:
  build:
    name: Build
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"job 'build' has no timeout-minutes"* ]]
}

@test "fails when the timeout value is not a positive integer" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [push]

jobs:
  build:
    runs-on: ubuntu-latest
    timeout-minutes: soon
    steps:
      - run: echo ok
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"must be a positive integer"* ]]
}

@test "fails when the timeout exceeds GitHub's 360-minute ceiling" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [push]

jobs:
  build:
    runs-on: ubuntu-latest
    timeout-minutes: 600
    steps:
      - run: echo ok
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"must be between 1 and 360"* ]]
}

@test "fails when a zero timeout is declared" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [push]

jobs:
  build:
    runs-on: ubuntu-latest
    timeout-minutes: 0
    steps:
      - run: echo ok
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"must be between 1 and 360"* ]]
}

@test "fails when a reusable-call job declares timeout-minutes" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [pull_request]

jobs:
  security:
    timeout-minutes: 15
    uses: ./.github/workflows/security.yml
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"calls a reusable workflow and must not declare timeout-minutes"* ]]
}

@test "reports an error when the workflow file does not exist" {
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF_DIR/does-not-exist.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "every real repository workflow job declares a sane timeout" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
