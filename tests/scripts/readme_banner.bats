#!/usr/bin/env bats
# Tests for scripts/check-readme-banner.sh — Issue #565.
#
# The validator keeps the README branding banner in place: one image line
# directly under the H1, alt text naming the project, and a hot-link to the
# hub's canonical per-repo preview so a hub re-render propagates without this
# repo committing any image. Synthetic fixtures in a temp directory exercise
# the behaviour, so the real README is never mutated.

load 'test_helper'

BANNER_URL="https://raw.githubusercontent.com/stSoftwareAU/NEAT-AI/Develop/docs/brand/social-previews/neat-ai-scorer.png"

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-readme-banner.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_DIR="$(mktemp -d)"
  export TMP_DIR

  write_readme "![NEAT-AI-scorer banner]($BANNER_URL)"
}

teardown() {
  rm -rf "$TMP_DIR"
}

# Stand-in for the real README: H1, banner line, then ordinary prose.
write_readme() {
  cat >"$TMP_DIR/README.md" <<EOF
# NEAT-AI-scorer

$1

Native **MSE scorer** CLI for NEAT-AI creatures.

## Build
EOF
}

run_check() {
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md"
}

@test "passes when the banner hot-links the hub preview under the H1" {
  run_check
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
}

@test "fails when the README has no banner under the H1" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# NEAT-AI-scorer

Native **MSE scorer** CLI for NEAT-AI creatures.
EOF
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"banner"* ]]
}

@test "fails when the banner sits below other prose instead of under the H1" {
  cat >"$TMP_DIR/README.md" <<EOF
# NEAT-AI-scorer

Native **MSE scorer** CLI for NEAT-AI creatures.

![NEAT-AI-scorer banner]($BANNER_URL)
EOF
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"banner"* ]]
}

@test "fails when the banner points at a repo-local image instead of the hub" {
  write_readme "![NEAT-AI-scorer banner](docs/brand/neat-ai-scorer.png)"
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"raw.githubusercontent.com"* ]]
}

@test "fails when the banner hot-links another repo's preview" {
  write_readme "![NEAT-AI-scorer banner](https://raw.githubusercontent.com/stSoftwareAU/NEAT-AI/Develop/docs/brand/social-previews/neat-ai-core.png)"
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"neat-ai-scorer.png"* ]]
}

@test "fails when the banner alt text is empty" {
  write_readme "![]($BANNER_URL)"
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"alt text"* ]]
}

@test "fails when the banner alt text does not name the project" {
  write_readme "![banner]($BANNER_URL)"
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"alt text"* ]]
}

@test "fails loud when the README is missing" {
  assert_missing_target_rejected "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/absent.md"
}

@test "rejects unknown arguments with a usage error" {
  assert_unknown_flag_rejected "$SCRIPT_UNDER_TEST"
}

@test "the real repository README satisfies the banner check" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
}
