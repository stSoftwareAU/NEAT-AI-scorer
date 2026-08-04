#!/usr/bin/env bats
# Tests for scripts/check-run-block-safety.sh — Issue #400.
#
# Exercises the run-block safety guard with synthetic workflow YAML in temporary
# directories so behaviour (exit codes, reported offenders) is verified
# end-to-end without mutating the real workflow files. Also asserts the real
# repository keeps every risk-bearing multi-line run: block under
# `set -euo pipefail`.

load 'test_helper'

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-run-block-safety.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# A symlink block WITHOUT the safety prefix (the drift the audit flagged).
write_unsafe_symlink_workflow() {
  cat >"$1" <<'EOF'
name: Unsafe
jobs:
  x:
    steps:
      - name: Link NEAT-AI-core sibling path expected by Cargo
        run: |
          if [ ! -e "$GITHUB_WORKSPACE/../NEAT-AI-core" ]; then
            ln -s "$GITHUB_WORKSPACE/NEAT-AI-core" "$GITHUB_WORKSPACE/../NEAT-AI-core"
          fi
EOF
}

# The same symlink block WITH the safety prefix.
write_safe_symlink_workflow() {
  cat >"$1" <<'EOF'
name: Safe
jobs:
  x:
    steps:
      - name: Link NEAT-AI-core sibling path expected by Cargo
        run: |
          set -euo pipefail
          if [ ! -e "$GITHUB_WORKSPACE/../NEAT-AI-core" ]; then
            ln -s "$GITHUB_WORKSPACE/NEAT-AI-core" "$GITHUB_WORKSPACE/../NEAT-AI-core"
          fi
EOF
}

# A find-*.sh driven block without the prefix (bash-syntax / ShellCheck shape).
write_unsafe_find_workflow() {
  cat >"$1" <<'EOF'
name: Find
jobs:
  x:
    steps:
      - name: Check bash script syntax
        run: |
          EXIT_CODE=0
          while IFS= read -r script; do
            bash -n "$script" || EXIT_CODE=1
          done < <(find . -name "*.sh" -not -path "./target/*")
          exit $EXIT_CODE
EOF
}

# A `sudo rm -rf` block without the prefix (free-disk-space shape).
write_unsafe_sudo_rm_workflow() {
  cat >"$1" <<'EOF'
name: FreeDisk
jobs:
  x:
    steps:
      - name: Free up runner disk space
        run: |
          df -h /
          sudo rm -rf /usr/share/dotnet
          df -h /
EOF
}

# A non-risky multi-line block: a single command spread over line continuations.
write_benign_continuation_workflow() {
  cat >"$1" <<'EOF'
name: Benign
jobs:
  x:
    steps:
      - name: Run linter
        run: |
          cargo clippy --all-targets --all-features -- \
            -D warnings
EOF
}

@test "passes when the symlink block opens with set -euo pipefail" {
  write_safe_symlink_workflow "$TMP_WF/ci.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
}

@test "fails when the symlink block omits set -euo pipefail" {
  write_unsafe_symlink_workflow "$TMP_WF/ci.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 1 ]
  [[ "$output" == *"FAIL"* ]]
  [[ "$output" == *"ci.yml"* ]]
}

@test "fails when a find-*.sh block omits set -euo pipefail" {
  write_unsafe_find_workflow "$TMP_WF/ci.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 1 ]
  [[ "$output" == *"FAIL"* ]]
}

@test "fails when a sudo rm -rf block omits set -euo pipefail" {
  write_unsafe_sudo_rm_workflow "$TMP_WF/ci.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 1 ]
  [[ "$output" == *"FAIL"* ]]
}

@test "reports every offending block across multiple files" {
  write_unsafe_symlink_workflow "$TMP_WF/security.yml"
  write_unsafe_sudo_rm_workflow "$TMP_WF/ci.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 1 ]
  [[ "$output" == *"security.yml"* ]]
  [[ "$output" == *"ci.yml"* ]]
}

@test "ignores a benign single-command continuation block" {
  write_benign_continuation_workflow "$TMP_WF/ci.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
}

@test "reports an error when the workflows directory does not exist" {
  assert_missing_target_rejected "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF/missing"
}

@test "unknown flag prints usage and exits non-zero" {
  assert_unknown_flag_rejected "$SCRIPT_UNDER_TEST"
}

@test "real repository keeps every risk-bearing run: block safe" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
