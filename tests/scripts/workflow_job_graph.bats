#!/usr/bin/env bats
# Tests for scripts/check-ci-job-graph.sh — Issue #23.
#
# Exercises the validator end-to-end with synthetic CI workflow YAML in
# temporary directories, plus one assertion against the real repo workflow
# so the enforced graph and the shipped workflow cannot drift apart.

load 'test_helper'

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-ci-job-graph.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF_DIR="$(mktemp -d)"
  TMP_WF="$TMP_WF_DIR/ci.yml"
  export TMP_WF_DIR TMP_WF
}

teardown() {
  rm -rf "$TMP_WF_DIR"
}

write_good_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: CI
on: [pull_request]
jobs:
  validation:
    name: Project Validation
    runs-on: ubuntu-latest
    steps:
      - run: echo ok

  quality:
    name: Quality Checks
    needs: [validation]
    runs-on: ubuntu-latest
    steps:
      - run: echo ok

  security:
    needs: [validation]
    if: github.event_name == 'pull_request'
    uses: ./.github/workflows/security.yml

  shell-checks:
    name: Shell Script Quality
    runs-on: ubuntu-latest
    steps:
      - run: echo ok

  spell-check:
    name: Spell Check
    runs-on: ubuntu-latest
    steps:
      - run: echo ok

  ci-required:
    name: CI Required Checks
    needs: [validation, quality, security, shell-checks, spell-check]
    if: always()
    runs-on: ubuntu-latest
    steps:
      - name: Verify all gating jobs succeeded
        run: |
          results=(
            "${{ needs.validation.result }}"
            "${{ needs.quality.result }}"
            "${{ needs.security.result }}"
            "${{ needs.shell-checks.result }}"
            "${{ needs.spell-check.result }}"
          )
          for r in "${results[@]}"; do
            if [[ "$r" != "success" && "$r" != "skipped" ]]; then
              exit 1
            fi
          done
EOF
}

@test "passes when the CI graph declares explicit needs and an aggregator" {
  write_good_workflow "$TMP_WF"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -eq 0 ]
  # Issue #360: prove every rule was individually evaluated and passed via the
  # machine-checkable "OK   " marker rather than pinning informational wording.
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 15 ]
}

@test "fails when the aggregator job is missing" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [pull_request]
jobs:
  validation:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
  quality:
    needs: [validation]
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
  security:
    needs: [validation]
    uses: ./.github/workflows/security.yml
  shell-checks:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
  spell-check:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"expected job 'ci-required' is not defined"* ]]
}

@test "fails when quality has no needs on validation" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [pull_request]
jobs:
  validation:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
  quality:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
  security:
    needs: [validation]
    uses: ./.github/workflows/security.yml
  shell-checks:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
  spell-check:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
  ci-required:
    needs: [validation, quality, security, shell-checks, spell-check]
    if: always()
    runs-on: ubuntu-latest
    steps:
      - run: echo "${{ needs.quality.result }}"
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"job 'quality' must declare needs: [validation]"* ]]
}

@test "fails when the aggregator is missing 'if: always()'" {
  write_good_workflow "$TMP_WF"
  # Strip the aggregator's `if: always()` line.
  awk '
    BEGIN { in_ci_required = 0 }
    /^  ci-required:/ { in_ci_required = 1 }
    in_ci_required && /^    if: always\(\)/ { next }
    { print }
  ' "$TMP_WF" >"$TMP_WF.new"
  mv "$TMP_WF.new" "$TMP_WF"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"must set 'if: always()'"* ]]
}

@test "fails when the aggregator does not inspect needs.*.result" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [pull_request]
jobs:
  validation:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
  quality:
    needs: [validation]
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
  security:
    needs: [validation]
    uses: ./.github/workflows/security.yml
  shell-checks:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
  spell-check:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
  ci-required:
    needs: [validation, quality, security, shell-checks, spell-check]
    if: always()
    runs-on: ubuntu-latest
    steps:
      - run: echo done
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"needs.<job>.result"* ]]
}

@test "fails when aggregator omits a gating job" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [pull_request]
jobs:
  validation:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
  quality:
    needs: [validation]
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
  security:
    needs: [validation]
    uses: ./.github/workflows/security.yml
  shell-checks:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
  spell-check:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
  ci-required:
    needs: [validation, quality, security, shell-checks]
    if: always()
    runs-on: ubuntu-latest
    steps:
      - run: echo "${{ needs.quality.result }}"
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"must list 'spell-check'"* ]]
}

@test "reports an error when the workflow file is missing" {
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF_DIR/nope.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  assert_unknown_flag_rejected "$SCRIPT_UNDER_TEST"
}

@test "real repository ci.yml satisfies the job graph rules" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
  [[ "$output" != *"FAIL"* ]]
}
