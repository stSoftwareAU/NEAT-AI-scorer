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
  family-sync-check:
    name: Verify canonical runlib.sh (fork PRs)
    runs-on: ubuntu-latest
    timeout-minutes: 10
    if: github.event.pull_request.head.repo.full_name != github.repository
    permissions:
      contents: read
    steps:
      - name: Verify
        run: |
          set -euo pipefail
          ./scripts/family-sync.sh --check

  family-sync:
    name: Sync canonical family scripts and move the neat-core pin
    runs-on: ubuntu-latest
    timeout-minutes: 10
    if: github.event.pull_request.head.repo.full_name == github.repository
    steps:
      - name: Refresh
        run: |
          set -euo pipefail
          ./scripts/family-sync.sh
      - name: Move the pin
        id: sync
        run: |
          set -euo pipefail
          ./scripts/family-pins.sh
          echo "changed=true" >>"$GITHUB_OUTPUT"
      - name: Commit and push
        if: steps.sync.outputs.changed == 'true'
        env:
          GH_PAT: ${{ secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN }}
        run: |
          set -euo pipefail
          GIT=/usr/bin/git
          "$GIT" add scripts/runlib.sh scripts/family-pins.sh rust_scorer/Cargo.toml Cargo.lock
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

@test "dropping the fork --check job fails" {
  write_valid_workflow
  sed '/family-sync.sh --check/d' "${TMP_DIR}/family-sync.yml" >"${TMP_DIR}/no-check.yml"
  run "$CHECK" --workflow "${TMP_DIR}/no-check.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"--check"* ]]
}

@test "dropping the family-pins.sh run fails — a behind pin would stay behind" {
  write_valid_workflow
  sed '/family-pins\.sh$/d' "${TMP_DIR}/family-sync.yml" >"${TMP_DIR}/no-pins.yml"
  run "$CHECK" --workflow "${TMP_DIR}/no-pins.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no 'scripts/family-pins.sh' run"* ]]
}

@test "a family-pins.sh run that is only promised in a comment fails" {
  write_valid_workflow
  sed 's|          ./scripts/family-pins.sh|          # runs ./scripts/family-pins.sh|' \
    "${TMP_DIR}/family-sync.yml" >"${TMP_DIR}/commented-pins.yml"
  run "$CHECK" --workflow "${TMP_DIR}/commented-pins.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no 'scripts/family-pins.sh' run"* ]]
}

@test "naming the pin paths outside the git add does not satisfy the staging rule" {
  write_valid_workflow
  # The change-detection step names both paths; the `git add` does not. What is
  # committed is what matters, so this must still fail.
  sed -e 's|          ./scripts/family-pins.sh|          ./scripts/family-pins.sh\n          git status --porcelain -- rust_scorer/Cargo.toml Cargo.lock|' \
    -e 's|"\$GIT" add .*|"$GIT" add scripts/runlib.sh scripts/family-pins.sh|' \
    "${TMP_DIR}/family-sync.yml" >"${TMP_DIR}/mentioned-only.yml"
  run "$CHECK" --workflow "${TMP_DIR}/mentioned-only.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"does not stage both"* ]]
}

@test "a git add spread over continuation lines still satisfies the staging rule" {
  write_valid_workflow
  sed 's|"\$GIT" add .*|"$GIT" add \\\n            scripts/runlib.sh scripts/family-pins.sh \\\n            rust_scorer/Cargo.toml Cargo.lock|' \
    "${TMP_DIR}/family-sync.yml" >"${TMP_DIR}/continued-add.yml"
  run "$CHECK" --workflow "${TMP_DIR}/continued-add.yml"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

@test "a commit that does not stage the moved pin fails" {
  write_valid_workflow
  sed 's|"\$GIT" add .*|"$GIT" add scripts/runlib.sh scripts/family-pins.sh|' \
    "${TMP_DIR}/family-sync.yml" >"${TMP_DIR}/unstaged-pin.yml"
  run "$CHECK" --workflow "${TMP_DIR}/unstaged-pin.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"does not stage both"* ]]
}

@test "a missing workflow file fails loud" {
  assert_missing_target_rejected "$CHECK" --workflow "${TMP_DIR}/absent.yml"
}

@test "an unknown flag prints usage and exits non-zero" {
  assert_unknown_flag_rejected "$CHECK"
}

@test "a run: block without strict bash fails" {
  write_valid_workflow
  sed '0,/          set -euo pipefail/s/          set -euo pipefail//' \
    "${TMP_DIR}/family-sync.yml" >"${TMP_DIR}/loose.yml"
  run "$CHECK" --workflow "${TMP_DIR}/loose.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"set -euo pipefail"* ]]
}

# The Issue #656 shape: the push step delegates to the shared composite action
# (Issue #641) instead of spelling out `git add` / `pull --rebase` in a run:.
write_delegated_workflow() {
  write_valid_workflow
  awk '/^      - name: Commit and push$/ { exit } { print }' \
    "${TMP_DIR}/family-sync.yml" >"${TMP_DIR}/delegated.yml"
  cat >>"${TMP_DIR}/delegated.yml" <<'YAML'
      - name: Commit and push
        if: steps.sync.outputs.changed == 'true'
        uses: ./.github/actions/push-with-app-token
        with:
          branch: ${{ github.event.pull_request.head.ref }}
          commit-message: |-
            chore: sync canonical family scripts and move the neat-core pin
          paths: |
            scripts/runlib.sh
            scripts/family-pins.sh
            rust_scorer/Cargo.toml
            Cargo.lock
          rebase: "true"
          app-client-id: ${{ secrets.ACTIONS_PUSH_APP_CLIENT_ID }}
          app-private-key: ${{ secrets.ACTIONS_PUSH_APP_PRIVATE_KEY }}
          fallback-token: ${{ secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN }}
YAML
}

@test "a push delegated to push-with-app-token passes every rule (Issue #656)" {
  write_delegated_workflow
  run "$CHECK" --workflow "${TMP_DIR}/delegated.yml"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

@test "a delegated push without rebase: true fails the rebase rule" {
  write_delegated_workflow
  sed '/rebase: "true"/d' "${TMP_DIR}/delegated.yml" >"${TMP_DIR}/delegated-no-rebase.yml"
  run "$CHECK" --workflow "${TMP_DIR}/delegated-no-rebase.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no rebase before push"* ]]
}

@test "a delegated push with rebase: false fails the rebase rule" {
  write_delegated_workflow
  sed 's/rebase: "true"/rebase: "false"/' "${TMP_DIR}/delegated.yml" >"${TMP_DIR}/delegated-false.yml"
  run "$CHECK" --workflow "${TMP_DIR}/delegated-false.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no rebase before push"* ]]
}

@test "a delegated rebase that is only promised in a comment fails" {
  write_delegated_workflow
  sed 's/          rebase: "true"/          # rebase: "true"/' \
    "${TMP_DIR}/delegated.yml" >"${TMP_DIR}/delegated-commented.yml"
  run "$CHECK" --workflow "${TMP_DIR}/delegated-commented.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no rebase before push"* ]]
}

@test "a rebase: true on some other step does not satisfy the rebase rule" {
  write_delegated_workflow
  sed '/rebase: "true"/d' "${TMP_DIR}/delegated.yml" >"${TMP_DIR}/elsewhere.yml"
  sed -i 's|^      - name: Refresh$|      - name: Refresh\n        with:\n          rebase: "true"|' \
    "${TMP_DIR}/elsewhere.yml"
  run "$CHECK" --workflow "${TMP_DIR}/elsewhere.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no rebase before push"* ]]
}

@test "delegated paths missing Cargo.lock fail the staging rule" {
  write_delegated_workflow
  sed '/^            Cargo\.lock$/d' "${TMP_DIR}/delegated.yml" >"${TMP_DIR}/delegated-no-lock.yml"
  run "$CHECK" --workflow "${TMP_DIR}/delegated-no-lock.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"does not stage both"* ]]
}

@test "delegation with no paths (commit -am) fails the staging rule" {
  write_delegated_workflow
  sed -e '/^          paths: |$/d' -e '/^            [A-Za-z_./]*$/d' \
    "${TMP_DIR}/delegated.yml" >"${TMP_DIR}/delegated-no-paths.yml"
  run "$CHECK" --workflow "${TMP_DIR}/delegated-no-paths.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"does not stage both"* ]]
}

@test "a workflow with no git add at all fails loud on the staging rule" {
  write_valid_workflow
  sed '/"\$GIT" add /d' "${TMP_DIR}/family-sync.yml" >"${TMP_DIR}/no-add.yml"
  run "$CHECK" --workflow "${TMP_DIR}/no-add.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"does not stage both"* ]]
}
