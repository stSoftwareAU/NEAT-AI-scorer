#!/usr/bin/env bats
# Tests for scripts/check-neat-core-composite-action.sh — Issues #401, #630.
#
# TEST MODIFICATION (Issue #630): `neat-core` is now pinned to a NEAT-AI-core
# release tag, which Cargo fetches itself, so the sibling checkout — and the
# `setup-neat-core` composite action that owned it — are retired. The guard
# therefore asserts the *absence* of that action and of any NEAT-AI-core
# checkout, plus the presence of the pin the retirement rests on. The cases
# that used to assert a well-formed composite action (`using: composite`, the
# sibling-link step, its `set -euo pipefail`) are gone with their subject: the
# file they parsed no longer exists.
#
# Exercises the guard end-to-end with synthetic workflow / manifest fixtures in
# temporary directories, plus assertions against the real repo so the enforced
# rule and the shipped files cannot drift apart.

load 'test_helper'

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-neat-core-composite-action.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP="$(mktemp -d)"
  ACTION_DIR="$TMP/.github/actions/setup-neat-core"
  WF_DIR="$TMP/.github/workflows"
  MANIFEST="$TMP/Cargo.toml"
  mkdir -p "$WF_DIR"
  export TMP ACTION_DIR WF_DIR MANIFEST
}

teardown() {
  rm -rf "$TMP"
}

# A workflow of the post-#630 shape: it checks the repo out and builds, with no
# sibling clone anywhere.
write_pinned_workflow() {
  cat >"$WF_DIR/ci.yml" <<'EOF'
name: CI
on: [pull_request]
jobs:
  quality:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@abc123  # v5
        with:
          persist-credentials: false
      - run: cargo build --workspace
EOF
}

# The crate manifest carrying the release-tag pin.
write_pinned_manifest() {
  cat >"$MANIFEST" <<'EOF'
[package]
name = "rust_scorer"
version = "1.0.0"

[dependencies]
neat-core = { git = "https://github.com/stSoftwareAU/NEAT-AI-core", tag = "v0.22.5" }
EOF
}

# The retired composite action, resurrected.
write_retired_action() {
  mkdir -p "$ACTION_DIR"
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
        path: NEAT-AI-core
        persist-credentials: false
EOF
}

run_guard() {
  run "$SCRIPT_UNDER_TEST" --action "$ACTION_DIR/action.yml" \
    --workflows "$WF_DIR" --manifest "$MANIFEST"
}

@test "passes when the composite is retired and the pin is declared" {
  write_pinned_workflow
  write_pinned_manifest
  run_guard
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
  [[ "$output" == *"retired composite action absent"* ]]
  [[ "$output" == *"no workflow checks out stSoftwareAU/NEAT-AI-core"* ]]
  [[ "$output" == *"pinned to a NEAT-AI-core release tag"* ]]
}

@test "fails when the retired composite action is resurrected" {
  write_pinned_workflow
  write_pinned_manifest
  write_retired_action
  run_guard
  [ "$status" -ne 0 ]
  [[ "$output" == *"composite action is retired"* ]]
}

@test "fails when a workflow still references the retired composite" {
  write_pinned_workflow
  write_pinned_manifest
  cat >"$WF_DIR/legacy.yml" <<'EOF'
name: Legacy
on: [pull_request]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Set up NEAT-AI-core sibling (path dependency for neat-core)
        uses: ./.github/actions/setup-neat-core
EOF
  run_guard
  [ "$status" -ne 0 ]
  [[ "$output" == *"references the retired"* ]]
  [[ "$output" == *"legacy.yml"* ]]
}

@test "fails when a workflow inlines a NEAT-AI-core checkout" {
  write_pinned_workflow
  write_pinned_manifest
  cat >"$WF_DIR/sibling.yml" <<'EOF'
name: Sibling
on: [pull_request]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout NEAT-AI-core
        uses: actions/checkout@abc123  # v5
        with:
          repository: stSoftwareAU/NEAT-AI-core
          path: NEAT-AI-core
EOF
  run_guard
  [ "$status" -ne 0 ]
  [[ "$output" == *"checks out stSoftwareAU/NEAT-AI-core"* ]]
  [[ "$output" == *"sibling.yml"* ]]
}

@test "fails when the manifest reverts to a path dependency" {
  write_pinned_workflow
  cat >"$MANIFEST" <<'EOF'
[dependencies]
neat-core = { path = "../../NEAT-AI-core/neat-core" }
EOF
  run_guard
  [ "$status" -ne 0 ]
  [[ "$output" == *"path dependency again"* ]]
}

@test "fails when the manifest carries a git dependency with no release tag" {
  write_pinned_workflow
  cat >"$MANIFEST" <<'EOF'
[dependencies]
neat-core = { git = "https://github.com/stSoftwareAU/NEAT-AI-core", branch = "Develop" }
EOF
  run_guard
  [ "$status" -ne 0 ]
  [[ "$output" == *"no 'neat-core = { git ="* ]]
}

@test "a missing manifest fails loud" {
  write_pinned_workflow
  assert_missing_target_rejected "$SCRIPT_UNDER_TEST" \
    --action "$ACTION_DIR/action.yml" --workflows "$WF_DIR" \
    --manifest "$TMP/absent.toml"
}

@test "unknown flag prints usage and exits non-zero" {
  assert_unknown_flag_rejected "$SCRIPT_UNDER_TEST"
}

@test "real repository satisfies the retirement guard" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
