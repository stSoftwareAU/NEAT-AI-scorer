#!/usr/bin/env bats
# Tests for scripts/lib/workflow-checks.sh — Issue #511.
#
# The `actions/checkout` pin-acceptance rule used to be re-implemented inline in
# six workflow validators and had drifted into three generations of the regex.
# These tests exercise the single shared helper directly: a caller sources the
# library, defines the `ok`/`fail` reporting contract, and calls
# `require_pinned_checkout` against synthetic workflow YAML.

setup() {
  LIB="${BATS_TEST_DIRNAME}/../../scripts/lib/workflow-checks.sh"
  TMP_WF="$(mktemp -d)"
  export LIB TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# Write a minimal workflow whose checkout step uses $1 as the ref.
write_workflow_with_ref() {
  cat >"$TMP_WF/wf.yml" <<EOF
name: Example
jobs:
  build:
    steps:
      - uses: actions/checkout@$1
EOF
}

# Run require_pinned_checkout in a subshell that provides the ok/fail contract
# the six validators define, mirroring how they call the helper.
run_helper() {
  run bash -c '
    set -euo pipefail
    EXIT_CODE=0
    fail() { echo "FAIL: $*" >&2; EXIT_CODE=1; }
    ok()   { echo "OK   : $*"; }
    source "$1"
    require_pinned_checkout "$2"
    exit "$EXIT_CODE"
  ' bash "$LIB" "$1"
}

@test "accepts a numeric major version pin" {
  write_workflow_with_ref "v5"
  run_helper "$TMP_WF/wf.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK   "* ]]
  [[ "$output" == *"actions/checkout pinned"* ]]
}

@test "accepts a 40-char SHA whose first character is a hex letter (Issue #136)" {
  # The pre-#136 `v?[0-9]+` regex rejected these; two validators still carried
  # it before Issue #511 unified the rule.
  write_workflow_with_ref "a1d282b9c3e4f5061728394a5b6c7d8e9f001122"
  run_helper "$TMP_WF/wf.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"actions/checkout pinned"* ]]
}

@test "accepts a 40-char SHA whose first character is a digit" {
  write_workflow_with_ref "93cb6efe18208431cddfb8368fd83d5badbf9bfd"
  run_helper "$TMP_WF/wf.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"actions/checkout pinned"* ]]
}

@test "accepts a SHA pin carrying a trailing version comment" {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  build:
    steps:
      - uses: actions/checkout@a1d282b9c3e4f5061728394a5b6c7d8e9f001122  # v5
EOF
  run_helper "$TMP_WF/wf.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"actions/checkout pinned"* ]]
}

@test "rejects a branch ref" {
  write_workflow_with_ref "main"
  run_helper "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/checkout"* ]]
  [[ "$output" == *"not pinned"* ]]
}

@test "rejects a truncated (short) SHA" {
  write_workflow_with_ref "a1d282b"
  run_helper "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not pinned"* ]]
}

@test "rejects an over-long hex ref (the word-boundary anchor)" {
  # 41 hex characters — a 40-char prefix match without the `\b` anchor would
  # wrongly accept this.
  write_workflow_with_ref "a1d282b9c3e4f5061728394a5b6c7d8e9f0011223"
  run_helper "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not pinned"* ]]
}

@test "rejects a tag ref that merely starts with digits" {
  write_workflow_with_ref "5-latest-branch"
  run_helper "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not pinned"* ]]
}

@test "reports a missing checkout step" {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  build:
    steps:
      - run: echo hello
EOF
  run_helper "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/checkout step missing"* ]]
}

@test "every checkout step must be pinned, not just one" {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  build:
    steps:
      - uses: actions/checkout@v5
      - uses: actions/checkout@main
EOF
  run_helper "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not pinned"* ]]
  [[ "$output" == *"main"* ]]
}

@test "passes when several checkout steps are all pinned" {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  build:
    steps:
      - uses: actions/checkout@v5
      - uses: actions/checkout@a1d282b9c3e4f5061728394a5b6c7d8e9f001122
EOF
  run_helper "$TMP_WF/wf.yml"
  [ "$status" -eq 0 ]
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 1 ]
}

@test "fails loudly when called without a workflow argument" {
  run bash -c '
    set -uo pipefail
    fail() { :; }
    ok() { :; }
    source "$1"
    require_pinned_checkout
  ' bash "$LIB"
  [ "$status" -eq 2 ]
  [[ "$output" == *"workflow path"* ]]
}

@test "fails loudly when the workflow file is unreadable" {
  run_helper "$TMP_WF/does-not-exist.yml"
  [ "$status" -eq 2 ]
  [[ "$output" == *"not readable"* ]]
}

@test "fails loudly when the caller has not defined ok/fail" {
  write_workflow_with_ref "v5"
  run bash -c '
    set -uo pipefail
    source "$1"
    require_pinned_checkout "$2"
  ' bash "$LIB" "$TMP_WF/wf.yml"
  [ "$status" -eq 2 ]
  [[ "$output" == *"ok()"* ]]
}
