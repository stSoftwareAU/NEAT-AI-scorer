#!/usr/bin/env bats
# Tests for scripts/check-milestone-branch-filter.sh — Issue #393.
#
# Exercises the milestone branch-filter validator with synthetic workflow YAML
# in temporary directories so behaviour (exit codes, reported failures) is
# verified end-to-end without depending on the real workflow file's state.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-milestone-branch-filter.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# Canonical workflow whose pull_request filter gates milestone branches. Failure
# tests mutate this fixture to drop or break the milestone glob.
write_gated_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Example

on:
  push:
    branches:
      - Develop
  pull_request:
    types: [opened, synchronize, reopened]
    branches:
      - Develop
      - milestone/*
  workflow_dispatch:

jobs:
  example:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
EOF
}

@test "passes when pull_request.branches includes milestone/*" {
  write_gated_workflow "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/example.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"includes 'milestone/*'"* ]]
}

@test "fails when the milestone/* glob is absent" {
  write_gated_workflow "$TMP_WF/example.yml"
  sed -i.bak '/- milestone\/\*/d' "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/example.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"must include 'milestone/*'"* ]]
}

@test "fails when there is no pull_request.branches filter at all" {
  cat >"$TMP_WF/example.yml" <<'EOF'
name: Example

on:
  pull_request:
    types: [opened, synchronize, reopened]
  workflow_dispatch:

jobs:
  example:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/example.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no on.pull_request.branches filter found"* ]]
}

@test "accepts the inline list form of branches" {
  cat >"$TMP_WF/example.yml" <<'EOF'
name: Example

on:
  pull_request:
    branches: [Develop, "milestone/*"]

jobs:
  example:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/example.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"includes 'milestone/*'"* ]]
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

@test "the repository CI workflow gates milestone PRs" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

# Issue #394 — the Gitleaks CI quality workflow must also gate milestone PRs.
@test "the repository Gitleaks workflow gates milestone PRs" {
  run "$SCRIPT_UNDER_TEST" --workflow "${BATS_TEST_DIRNAME}/../../.github/workflows/gitleaks.yml"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

# Issue #395 — the Markdown Lint CI quality workflow must also gate milestone
# PRs. A bare `["*"]` filter matches only slash-free branch names, so
# `milestone/<slug>` PRs slip through unlinted; the filter must include a
# `milestone/*` glob.
@test "the repository Markdown Lint workflow gates milestone PRs" {
  run "$SCRIPT_UNDER_TEST" --workflow "${BATS_TEST_DIRNAME}/../../.github/workflows/markdown-lint.yml"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

# Issue #396 — the Semgrep SAST CI quality workflow must also gate milestone
# PRs. A bare `["*"]` filter matches only slash-free branch names, so
# `milestone/<slug>` PRs slip through unscanned; the filter must include a
# `milestone/*` glob.
@test "the repository Semgrep workflow gates milestone PRs" {
  run "$SCRIPT_UNDER_TEST" --workflow "${BATS_TEST_DIRNAME}/../../.github/workflows/semgrep.yml"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
