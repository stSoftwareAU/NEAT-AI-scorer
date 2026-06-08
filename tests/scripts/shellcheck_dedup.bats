#!/usr/bin/env bats
# Tests for scripts/check-shellcheck-dedup.sh — Issue #157.
#
# Exercises the ShellCheck dedup guard with synthetic workflow YAML in
# temporary directories so behaviour (exit codes, reported failures) is
# verified end-to-end without mutating the real workflow files. Also asserts
# the real repository keeps ShellCheck in exactly one workflow.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-shellcheck-dedup.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# Write a workflow that invokes ludeeus/action-shellcheck.
write_shellcheck_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Example
on:
  pull_request:
jobs:
  shellcheck:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - name: Run ShellCheck
        uses: ludeeus/action-shellcheck@00cae500b08a931fb5698e11e79bfbd38e612a38  # v2.0.0
        with:
          scandir: "."
          severity: warning
EOF
}

# Write a workflow that invokes the pre-installed shellcheck binary directly
# in a run step (the form ci.yml uses after PR #184 dropped the action).
write_shellcheck_run_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Example Run
on:
  pull_request:
jobs:
  shellcheck:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - name: Run ShellCheck (pre-installed)
        run: |
          shellcheck --version
          shellcheck --severity=warning script.sh
EOF
}

# Write a workflow that does NOT invoke ShellCheck (only mentions it in prose).
write_unrelated_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Other
on:
  pull_request:
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      # Note: ShellCheck via ludeeus/action-shellcheck runs elsewhere.
      - uses: actions/checkout@v5
EOF
}

@test "passes when exactly one workflow invokes ShellCheck" {
  write_shellcheck_workflow "$TMP_WF/ci.yml"
  write_unrelated_workflow "$TMP_WF/build.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"exactly one workflow"* ]]
  [[ "$output" == *"ci.yml"* ]]
}

@test "counts a direct shellcheck run step as an invocation" {
  write_shellcheck_run_workflow "$TMP_WF/ci.yml"
  write_unrelated_workflow "$TMP_WF/build.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"exactly one workflow"* ]]
  [[ "$output" == *"ci.yml"* ]]
}

@test "fails when a run step duplicates the action invocation" {
  write_shellcheck_workflow "$TMP_WF/ci.yml"
  write_shellcheck_run_workflow "$TMP_WF/shellcheck.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"duplicated across 2 workflows"* ]]
}

@test "fails when ShellCheck is duplicated across two workflows" {
  write_shellcheck_workflow "$TMP_WF/ci.yml"
  write_shellcheck_workflow "$TMP_WF/shellcheck.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"duplicated across 2 workflows"* ]]
  [[ "$output" == *"ci.yml"* ]]
  [[ "$output" == *"shellcheck.yml"* ]]
}

@test "fails when no workflow invokes ShellCheck" {
  write_unrelated_workflow "$TMP_WF/build.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"coverage is missing"* ]]
}

@test "prose mention of the action does not count as an invocation" {
  write_shellcheck_workflow "$TMP_WF/ci.yml"
  write_unrelated_workflow "$TMP_WF/comment-only.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"exactly one workflow"* ]]
}

@test "reports an error when the workflows directory does not exist" {
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF/missing"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "real repository keeps ShellCheck in exactly one workflow" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
  [[ "$output" == *"ci.yml"* ]]
}
