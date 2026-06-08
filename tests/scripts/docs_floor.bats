#!/usr/bin/env bats
# Tests for Issue #178 — complete the repository "docs floor" by adding
# CONTRIBUTING.md and CHANGELOG.md and enforcing them as required files in CI.

setup() {
  REPO_ROOT="${BATS_TEST_DIRNAME}/../.."
  export REPO_ROOT
  export LC_ALL="${LC_ALL:-en_US.UTF-8}"
  CI_WORKFLOW="$REPO_ROOT/.github/workflows/ci.yml"
  export CI_WORKFLOW
}

@test "CONTRIBUTING.md exists at the repository root" {
  [ -f "$REPO_ROOT/CONTRIBUTING.md" ]
}

@test "CHANGELOG.md exists at the repository root" {
  [ -f "$REPO_ROOT/CHANGELOG.md" ]
}

@test "CONTRIBUTING.md documents the local quality gate" {
  run grep -F './quality.sh' "$REPO_ROOT/CONTRIBUTING.md"
  [ "$status" -eq 0 ]
}

@test "CHANGELOG.md follows Keep a Changelog with an Unreleased section" {
  run grep -F '[Unreleased]' "$REPO_ROOT/CHANGELOG.md"
  [ "$status" -eq 0 ]
  run grep -Fi 'keep a changelog' "$REPO_ROOT/CHANGELOG.md"
  [ "$status" -eq 0 ]
}

@test "ci.yml required-files check lists CONTRIBUTING.md" {
  run grep -F '"CONTRIBUTING.md"' "$CI_WORKFLOW"
  [ "$status" -eq 0 ]
}

@test "ci.yml required-files check lists CHANGELOG.md" {
  run grep -F '"CHANGELOG.md"' "$CI_WORKFLOW"
  [ "$status" -eq 0 ]
}
