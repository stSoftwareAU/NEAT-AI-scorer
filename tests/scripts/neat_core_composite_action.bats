#!/usr/bin/env bats
# Tests for scripts/check-neat-core-composite-action.sh — Issue #401.
#
# Exercises the guard end-to-end with synthetic composite-action + workflow
# fixtures in temporary directories, plus assertions against the real repo so
# the enforced rule and the shipped files cannot drift apart.

load 'test_helper'

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-neat-core-composite-action.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP="$(mktemp -d)"
  ACTION_DIR="$TMP/.github/actions/setup-neat-core"
  WF_DIR="$TMP/.github/workflows"
  mkdir -p "$ACTION_DIR" "$WF_DIR"
  export TMP ACTION_DIR WF_DIR
}

teardown() {
  rm -rf "$TMP"
}

# A valid composite action: composite runner, NEAT-AI-core checkout, sibling
# link step whose run block opens with `set -euo pipefail`.
write_good_action() {
  cat >"$ACTION_DIR/action.yml" <<'EOF'
name: Set up NEAT-AI-core sibling
description: Checkout + symlink.
runs:
  using: composite
  steps:
    - name: Checkout NEAT-AI-core (path dependency for neat-core)
      uses: actions/checkout@abc123  # v5
      with:
        repository: stSoftwareAU/NEAT-AI-core
        ref: Develop
        path: NEAT-AI-core
        persist-credentials: false
    - name: Link NEAT-AI-core sibling path expected by Cargo
      shell: bash
      run: |
        set -euo pipefail
        if [ ! -e "$GITHUB_WORKSPACE/../NEAT-AI-core" ]; then
          ln -s "$GITHUB_WORKSPACE/NEAT-AI-core" "$GITHUB_WORKSPACE/../NEAT-AI-core"
        fi
EOF
}

# A workflow that consumes the composite action.
write_consumer_workflow() {
  cat >"$WF_DIR/ci.yml" <<'EOF'
name: CI
on: [push]
jobs:
  quality:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@abc123  # v5
      - name: Set up NEAT-AI-core sibling (path dependency for neat-core)
        uses: ./.github/actions/setup-neat-core
EOF
}

run_guard() {
  run "$SCRIPT_UNDER_TEST" --action "$ACTION_DIR/action.yml" --workflows "$WF_DIR"
}

@test "passes when the composite exists and every workflow uses it" {
  write_good_action
  write_consumer_workflow
  run_guard
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
  [[ "$output" == *"composite action present"* ]]
  [[ "$output" == *"no workflow inlines a NEAT-AI-core checkout"* ]]
}

@test "fails when the composite action file is missing" {
  write_consumer_workflow
  run_guard
  [ "$status" -ne 0 ]
  [[ "$output" == *"composite action not found"* ]]
}

@test "fails when the action is not a composite action" {
  write_good_action
  # Break the `using: composite` declaration.
  sed -i.bak 's|using: composite|using: node20|' "$ACTION_DIR/action.yml"
  write_consumer_workflow
  run_guard
  [ "$status" -ne 0 ]
  [[ "$output" == *"not a composite action"* ]]
}

@test "fails when the composite symlink block omits set -euo pipefail" {
  write_good_action
  # Drop the safety prefix from the symlink run block.
  sed -i.bak '/set -euo pipefail/d' "$ACTION_DIR/action.yml"
  write_consumer_workflow
  run_guard
  [ "$status" -ne 0 ]
  [[ "$output" == *"set -euo pipefail"* ]]
}

@test "fails when a workflow still inlines a NEAT-AI-core checkout" {
  write_good_action
  write_consumer_workflow
  cat >"$WF_DIR/legacy.yml" <<'EOF'
name: Legacy
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout NEAT-AI-core (path dependency for neat-core)
        uses: actions/checkout@abc123  # v5
        with:
          repository: stSoftwareAU/NEAT-AI-core
          path: NEAT-AI-core
EOF
  run_guard
  [ "$status" -ne 0 ]
  [[ "$output" == *"inlines a NEAT-AI-core checkout"* ]]
  [[ "$output" == *"legacy.yml"* ]]
}

@test "fails when no workflow references the composite action" {
  write_good_action
  cat >"$WF_DIR/unrelated.yml" <<'EOF'
name: Unrelated
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@abc123  # v5
EOF
  run_guard
  [ "$status" -ne 0 ]
  [[ "$output" == *"no workflow references"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  assert_unknown_flag_rejected "$SCRIPT_UNDER_TEST"
}

@test "real repository satisfies the composite-action guard" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
