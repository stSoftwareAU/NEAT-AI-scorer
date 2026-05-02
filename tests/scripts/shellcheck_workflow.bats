#!/usr/bin/env bats
# Tests for scripts/check-shellcheck-workflow.sh — Issue #67.
#
# Exercises the standalone ShellCheck Lint workflow validator with synthetic
# workflow YAML in temporary directories so behaviour (exit codes, reported
# failures) is verified end-to-end without mutating the real workflow file.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-shellcheck-workflow.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# Canonical hardened workflow. Failure tests mutate this fixture to drop or
# break one rule at a time.
write_shellcheck_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: ShellCheck

on:
  pull_request:
    branches: ["*"]

permissions:
  contents: read

jobs:
  shellcheck:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - name: Run ShellCheck
        uses: ludeeus/action-shellcheck@2.0.0
        with:
          scandir: "."
          severity: warning
          ignore_paths: "target .git"
        env:
          SHELLCHECK_OPTS: -s bash
EOF
}

@test "passes on the canonical fixture" {
  write_shellcheck_workflow "$TMP_WF/shellcheck.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/shellcheck.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"triggers on pull_request"* ]]
  [[ "$output" == *"permissions block grants only contents: read"* ]]
  [[ "$output" == *"actions/checkout pinned"* ]]
  [[ "$output" == *"ludeeus/action-shellcheck pinned"* ]]
  [[ "$output" == *"severity"* ]]
}

@test "fails when the workflow is not triggered on pull_request" {
  write_shellcheck_workflow "$TMP_WF/shellcheck.yml"
  python3 - "$TMP_WF/shellcheck.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("  pull_request:\n    branches: [\"*\"]\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/shellcheck.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not triggered on pull_request"* ]]
}

@test "fails when the permissions block is missing" {
  write_shellcheck_workflow "$TMP_WF/shellcheck.yml"
  python3 - "$TMP_WF/shellcheck.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("permissions:\n  contents: read\n\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/shellcheck.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no 'permissions: contents: read'"* ]]
}

@test "fails when actions/checkout is unpinned" {
  write_shellcheck_workflow "$TMP_WF/shellcheck.yml"
  sed -i.bak 's|actions/checkout@v5|actions/checkout@main|' "$TMP_WF/shellcheck.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/shellcheck.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/checkout"* ]]
  [[ "$output" == *"not pinned"* ]]
}

@test "fails when ludeeus/action-shellcheck pins to @master" {
  write_shellcheck_workflow "$TMP_WF/shellcheck.yml"
  sed -i.bak 's|ludeeus/action-shellcheck@2.0.0|ludeeus/action-shellcheck@master|' "$TMP_WF/shellcheck.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/shellcheck.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"ludeeus/action-shellcheck"* ]]
  [[ "$output" == *"not pinned"* ]]
}

@test "fails when ludeeus/action-shellcheck step is missing" {
  write_shellcheck_workflow "$TMP_WF/shellcheck.yml"
  sed -i.bak '/ludeeus\/action-shellcheck/d' "$TMP_WF/shellcheck.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/shellcheck.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"ludeeus/action-shellcheck"* ]]
  [[ "$output" == *"missing"* ]]
}

@test "fails when severity is not declared" {
  write_shellcheck_workflow "$TMP_WF/shellcheck.yml"
  sed -i.bak '/severity:/d' "$TMP_WF/shellcheck.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/shellcheck.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"severity"* ]]
}

@test "reports an error when the workflow file does not exist" {
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/does-not-exist.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "real repository shellcheck workflow satisfies every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
