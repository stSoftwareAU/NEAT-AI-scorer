#!/usr/bin/env bats
# Tests for scripts/check-bot-push-token.sh (Issues #435, #498).
#
# Issue #435 established that bot pushes must not be attributed to
# GITHUB_TOKEN. Issue #498 narrowed *which* credential satisfies that:
# a short-lived installation token minted per run by a GitHub App and scoped
# to `contents: write` on this repository only, with the long-lived org-level
# ACTIONS_PUSH PAT kept only as a fallback until an org admin creates the App.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-bot-push-token.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"
  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# A workflow that satisfies the Issue #498 policy in full.
write_compliant_workflow() {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  push:
    steps:
      - name: Mint repo-scoped push token
        id: push-token
        uses: actions/create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1  # v3
        with:
          client-id: ${{ secrets.ACTIONS_PUSH_APP_CLIENT_ID }}
          private-key: ${{ secrets.ACTIONS_PUSH_APP_PRIVATE_KEY }}
          repositories: ${{ github.event.repository.name }}
          permission-contents: write
      - name: Push
        env:
          GH_PAT: ${{ steps.push-token.outputs.token || secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN }}
        run: git push
EOF
}

@test "passes when the push token is a repo-scoped app token with PAT fallback" {
  write_compliant_workflow
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/wf.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
  [[ "$output" != *"FAIL"* ]]
}

# Issue #498 changed the policy: the org-level PAT alone is no longer
# sufficient, because anything that reaches it gains the PAT's full
# organisation scope. This test previously asserted the opposite (a bare
# ACTIONS_PUSH || GITHUB_TOKEN chain passed); it is inverted deliberately.
@test "fails when the push relies on the org-level PAT alone" {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  push:
    steps:
      - name: Push
        env:
          GH_PAT: ${{ secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN }}
        run: git push
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"create-github-app-token"* ]]
}

@test "fails when workflow only uses GITHUB_TOKEN" {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  push:
    steps:
      - name: Checkout
        uses: actions/checkout@v4
        with:
          token: ${{ secrets.GITHUB_TOKEN }}
      - name: Push
        run: git push
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"ACTIONS_PUSH"* ]]
}

@test "fails when the app token action is not SHA-pinned" {
  write_compliant_workflow
  sed -i.bak 's%create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1%create-github-app-token@v3%' "$TMP_WF/wf.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"SHA-pinned"* ]]
}

@test "fails when the minted token is not narrowed to contents: write" {
  write_compliant_workflow
  sed -i.bak '/permission-contents: write/d' "$TMP_WF/wf.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"permission-contents: write"* ]]
}

@test "fails when the minted token is not scoped to a single repository" {
  write_compliant_workflow
  sed -i.bak '/repositories:/d' "$TMP_WF/wf.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"repositories:"* ]]
}

@test "fails when the mint step widens scope with an owner input" {
  write_compliant_workflow
  sed -i.bak 's%          repositories:%          owner: ${{ github.repository_owner }}\
          repositories:%' "$TMP_WF/wf.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"owner:"* ]]
}

@test "fails when the push step ignores the minted token" {
  write_compliant_workflow
  sed -i.bak 's%GH_PAT: ${{ steps.push-token.outputs.token || secrets.ACTIONS_PUSH %GH_PAT: ${{ secrets.ACTIONS_PUSH %' "$TMP_WF/wf.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"outputs.token"* ]]
}

@test "fails when the ACTIONS_PUSH fallback is dropped" {
  write_compliant_workflow
  sed -i.bak 's%steps.push-token.outputs.token || secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN%steps.push-token.outputs.token%' "$TMP_WF/wf.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"ACTIONS_PUSH"* ]]
}

@test "reports a missing workflow file instead of passing silently" {
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/absent.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "shipped auto-format and version-increment workflows validate cleanly" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" == *"auto-format.yml"* ]]
  [[ "$output" == *"version-increment.yml"* ]]
  [[ "$output" != *"FAIL"* ]]
}
