#!/usr/bin/env bats
# Tests for scripts/check-family-sync-workflow.sh (Issue #629).
#
# The guard is driven against fixture workflows so each rule is exercised on
# its own, plus one run against the shipped .github/workflows/family-sync.yml.

load 'test_helper'

setup() {
  CHECK="${BATS_TEST_DIRNAME}/../../scripts/check-family-sync-workflow.sh"
  [ -x "$CHECK" ] || chmod +x "$CHECK"
  SHIPPED="${BATS_TEST_DIRNAME}/../../.github/workflows/family-sync.yml"
  TMP_DIR="$(mktemp -d)"
  export TMP_DIR SHIPPED
}

teardown() {
  rm -rf "$TMP_DIR"
}

# A workflow that satisfies every rule; callers mutate the copy they need.
write_valid_workflow() {
  cat >"${TMP_DIR}/family-sync.yml" <<'EOF'
name: Family Sync

on:
  pull_request:
    types: [opened, synchronize, reopened]
    branches:
      - Develop
      - "milestone/**"

permissions:
  contents: write

concurrency:
  group: family-sync-${{ github.ref }}
  cancel-in-progress: true

jobs:
  family-sync:
    name: Sync canonical runlib.sh
    runs-on: ubuntu-latest
    timeout-minutes: 10
    if: github.event.pull_request.head.repo.full_name == github.repository
    steps:
      - name: Refresh
        id: sync
        run: |
          set -euo pipefail
          ./scripts/family-sync.sh
          echo "changed=true" >>"$GITHUB_OUTPUT"
      - name: Commit and push
        if: steps.sync.outputs.changed == 'true'
        env:
          GH_PAT: ${{ secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN }}
        run: |
          set -euo pipefail
          GIT=/usr/bin/git
          "$GIT" pull --rebase origin main
          "$GIT" push origin HEAD
EOF
}

@test "the shipped family-sync workflow validates cleanly" {
  run "$CHECK" --workflow "$SHIPPED"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

@test "a valid fixture passes every rule" {
  write_valid_workflow
  run "$CHECK" --workflow "${TMP_DIR}/family-sync.yml"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

@test "a renamed job fails — the audited job name must not drift" {
  write_valid_workflow
  sed 's/^  family-sync:/  sync-runlib:/' "${TMP_DIR}/family-sync.yml" >"${TMP_DIR}/renamed.yml"
  run "$CHECK" --workflow "${TMP_DIR}/renamed.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no 'family-sync:' job"* ]]
}

@test "a branch filter that misses milestone PRs fails" {
  write_valid_workflow
  sed '/milestone/d' "${TMP_DIR}/family-sync.yml" >"${TMP_DIR}/no-milestone.yml"
  run "$CHECK" --workflow "${TMP_DIR}/no-milestone.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"misses milestone"* ]]
}

@test "dropping the family-sync.sh invocation fails" {
  write_valid_workflow
  sed 's|\./scripts/family-sync\.sh|curl -o scripts/runlib.sh https://example.invalid|' \
    "${TMP_DIR}/family-sync.yml" >"${TMP_DIR}/inline.yml"
  run "$CHECK" --workflow "${TMP_DIR}/inline.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"family-sync.sh"* ]]
}

@test "dropping the rebase before push fails" {
  write_valid_workflow
  sed '/pull --rebase/d' "${TMP_DIR}/family-sync.yml" >"${TMP_DIR}/no-rebase.yml"
  run "$CHECK" --workflow "${TMP_DIR}/no-rebase.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no rebase before push"* ]]
}

@test "write-all permissions fail" {
  write_valid_workflow
  sed 's/^permissions:/permissions: write-all #/' "${TMP_DIR}/family-sync.yml" >"${TMP_DIR}/write-all.yml"
  run "$CHECK" --workflow "${TMP_DIR}/write-all.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"write-all"* ]]
}

@test "a push trigger fails — the sync must not commit onto Develop" {
  write_valid_workflow
  sed 's/^  pull_request:/  push:\n    branches: [Develop]\n  pull_request:/' \
    "${TMP_DIR}/family-sync.yml" >"${TMP_DIR}/push.yml"
  run "$CHECK" --workflow "${TMP_DIR}/push.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"push trigger present"* ]]
}

@test "a missing workflow file fails loud" {
  run "$CHECK" --workflow "${TMP_DIR}/absent.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}
