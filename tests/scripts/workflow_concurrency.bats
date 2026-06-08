#!/usr/bin/env bats
# Tests for scripts/check-workflow-concurrency.sh — Issue #156.
#
# Exercises the concurrency-group validator with synthetic workflow YAML in
# temporary directories so behaviour (exit codes, reported failures) is
# verified end-to-end without mutating the real workflow files.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-workflow-concurrency.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# Canonical hardened workflow. Failure tests mutate this fixture to drop or
# break one rule at a time.
write_hardened_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Example

on:
  pull_request:
    branches: ["*"]

permissions:
  contents: read

concurrency:
  group: example-${{ github.ref }}
  cancel-in-progress: true

jobs:
  example:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
EOF
}

@test "passes on the canonical hardened fixture" {
  write_hardened_workflow "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/example.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"declares a top-level concurrency block"* ]]
  [[ "$output" == *"keyed by github.ref"* ]]
  [[ "$output" == *"cancel-in-progress is true"* ]]
}

@test "fails when the concurrency block is missing" {
  write_hardened_workflow "$TMP_WF/example.yml"
  python3 - "$TMP_WF/example.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace(
    "concurrency:\n  group: example-${{ github.ref }}\n  cancel-in-progress: true\n\n",
    "",
)
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/example.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no top-level 'concurrency:' block"* ]]
}

@test "fails when the group is not keyed by github.ref" {
  write_hardened_workflow "$TMP_WF/example.yml"
  sed -i.bak 's|group: example-${{ github.ref }}|group: example-static|' "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/example.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not keyed by"* ]]
}

@test "fails when cancel-in-progress is not true" {
  write_hardened_workflow "$TMP_WF/example.yml"
  sed -i.bak 's|cancel-in-progress: true|cancel-in-progress: false|' "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/example.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"cancel-in-progress is not set to true"* ]]
}

@test "accepts the github.workflow-keyed group form" {
  write_hardened_workflow "$TMP_WF/example.yml"
  sed -i.bak 's|group: example-${{ github.ref }}|group: ${{ github.workflow }}-${{ github.ref }}|' "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/example.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"keyed by github.ref"* ]]
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

@test "every pile-up-prone repository workflow declares a concurrency group" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
