#!/usr/bin/env bats
# Tests for scripts/check-dependency-review-workflow.sh — Issue #62.
#
# Exercises the Dependency Review workflow validator with synthetic
# workflow YAML in temporary directories so behaviour (exit codes, reported
# failures) is verified end-to-end without mutating the real workflow file.

load 'test_helper'

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-dependency-review-workflow.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# Canonical hardened workflow. Failure tests mutate this fixture to drop or
# break one rule at a time.
write_dependency_review_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Dependency Review

# Standalone Dependency Review workflow (Issue #62).

on:
  pull_request:
    branches: ["*", "milestone/**"]

permissions:
  contents: read

jobs:
  dependency-review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: actions/dependency-review-action@v4
EOF
}

@test "passes on the canonical fixture" {
  write_dependency_review_workflow "$TMP_WF/dependency-review.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/dependency-review.yml"
  [ "$status" -eq 0 ]
  # Issue #360: prove every rule was individually evaluated and passed via the
  # machine-checkable "OK   " marker rather than pinning informational wording.
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 5 ]
}

@test "fails when the pull_request filter omits milestone branches (Issue #399)" {
  write_dependency_review_workflow "$TMP_WF/dependency-review.yml"
  sed -i.bak 's|branches: \["\*", "milestone/\*\*"\]|branches: ["*"]|' "$TMP_WF/dependency-review.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/dependency-review.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"omits milestone/*"* ]]
}

@test "fails when the workflow is not triggered on pull_request" {
  write_dependency_review_workflow "$TMP_WF/dependency-review.yml"
  python3 - "$TMP_WF/dependency-review.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("on:\n  pull_request:\n    branches: [\"*\", \"milestone/**\"]\n", "on:\n  workflow_dispatch:\n")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/dependency-review.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not triggered on pull_request"* ]]
}

@test "fails when the permissions block is missing" {
  write_dependency_review_workflow "$TMP_WF/dependency-review.yml"
  python3 - "$TMP_WF/dependency-review.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("permissions:\n  contents: read\n\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/dependency-review.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no 'permissions: contents: read'"* ]]
}

@test "fails when actions/checkout is unpinned" {
  write_dependency_review_workflow "$TMP_WF/dependency-review.yml"
  sed -i.bak 's|actions/checkout@v5|actions/checkout@main|' "$TMP_WF/dependency-review.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/dependency-review.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/checkout"* ]]
  [[ "$output" == *"not pinned"* ]]
}

@test "fails when actions/checkout is missing" {
  write_dependency_review_workflow "$TMP_WF/dependency-review.yml"
  sed -i.bak '/actions\/checkout/d' "$TMP_WF/dependency-review.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/dependency-review.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/checkout"* ]]
  [[ "$output" == *"missing"* ]]
}

@test "fails when actions/dependency-review-action is missing" {
  write_dependency_review_workflow "$TMP_WF/dependency-review.yml"
  sed -i.bak '/actions\/dependency-review-action/d' "$TMP_WF/dependency-review.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/dependency-review.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/dependency-review-action"* ]]
  [[ "$output" == *"missing"* ]]
}

@test "fails when actions/dependency-review-action is unpinned" {
  write_dependency_review_workflow "$TMP_WF/dependency-review.yml"
  sed -i.bak 's|actions/dependency-review-action@v4|actions/dependency-review-action@main|' "$TMP_WF/dependency-review.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/dependency-review.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/dependency-review-action"* ]]
  [[ "$output" == *"not pinned"* ]]
}

@test "reports an error when the workflow file does not exist" {
  assert_missing_target_rejected "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/does-not-exist.yml"
}

@test "unknown flag prints usage and exits non-zero" {
  assert_unknown_flag_rejected "$SCRIPT_UNDER_TEST"
}

@test "real repository dependency-review workflow satisfies every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
