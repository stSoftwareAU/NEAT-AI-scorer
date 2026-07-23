#!/usr/bin/env bats
# Tests for scripts/check-bot-push-token.sh (Issue #435).

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-bot-push-token.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"
  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

@test "passes when workflow uses ACTIONS_PUSH with GITHUB_TOKEN fallback" {
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
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
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

@test "shipped auto-format and version-increment workflows validate cleanly" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" == *"auto-format.yml"* ]]
  [[ "$output" == *"version-increment.yml"* ]]
  [[ "$output" != *"FAIL"* ]]
}
