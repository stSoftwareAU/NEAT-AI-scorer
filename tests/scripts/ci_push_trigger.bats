#!/usr/bin/env bats
# Tests for scripts/check-ci-push-trigger.sh — Issue #370.
#
# Exercises the push-trigger validator with synthetic workflow YAML in
# temporary directories so behaviour (exit codes, reported failures) is
# verified end-to-end without depending on the real workflow file's state.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-ci-push-trigger.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# Canonical fixed workflow: a checker that gates the PR only, with no push
# trigger. Failure tests mutate this fixture to reintroduce push-to-Develop.
write_pr_only_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: CI

on:
  pull_request:
    types: [opened, synchronize, reopened]
    branches:
      - Develop
      - milestone/*
  workflow_dispatch:

jobs:
  quality:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
EOF
}

@test "passes when there is no push trigger (checker gates the PR only)" {
  write_pr_only_workflow "$TMP_WF/ci.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/ci.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"no push trigger"* ]]
}

@test "fails when the push trigger targets the default branch Develop (block form)" {
  cat >"$TMP_WF/ci.yml" <<'EOF'
name: CI

on:
  push:
    branches:
      - Develop
    paths-ignore:
      - "**.md"
  pull_request:
    branches:
      - Develop
  workflow_dispatch:

jobs:
  quality:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/ci.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"must not re-run on push to Develop"* ]]
}

@test "fails when the push trigger targets Develop in inline-list form" {
  cat >"$TMP_WF/ci.yml" <<'EOF'
name: CI

on:
  push:
    branches: [main, Develop]
  pull_request:
    branches: [Develop, "milestone/*"]

jobs:
  quality:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/ci.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"Develop"* ]]
}

@test "passes when push targets only non-default branches" {
  cat >"$TMP_WF/ci.yml" <<'EOF'
name: CI

on:
  push:
    branches: [main, master]
  pull_request:
    branches:
      - Develop
  workflow_dispatch:

jobs:
  quality:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/ci.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"does not target the default branch Develop"* ]]
}

@test "does not confuse the pull_request Develop filter for a push trigger" {
  # pull_request legitimately lists Develop; only the push filter must not.
  write_pr_only_workflow "$TMP_WF/ci.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/ci.yml"
  [ "$status" -eq 0 ]
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

@test "the repository CI workflow does not re-trigger on push to Develop" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
