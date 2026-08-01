#!/usr/bin/env bats
# Tests for scripts/check-push-step-hardening.sh (Issue #497).

setup() {
  CHECK="${BATS_TEST_DIRNAME}/../../scripts/check-push-step-hardening.sh"
  [ -x "$CHECK" ] || chmod +x "$CHECK"
  TMP_WF="$(mktemp -d)"
  export TMP_WF CHECK
}

teardown() {
  rm -rf "$TMP_WF"
}

# A hardened PAT-bearing push step: absolute git and base64 paths, hooks
# disabled on every invocation, and no repository script executed with the
# PAT in scope.
write_hardened_workflow() {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  push:
    runs-on: ubuntu-latest
    steps:
      - name: Run PR-head code
        run: |
          set -euo pipefail
          ./scripts/auto-format.sh --check-changes
      - name: Commit and push
        env:
          PR_HEAD_REF: main
          GH_PAT: ${{ secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN }}
        run: |
          set -euo pipefail
          GIT=/usr/bin/git
          BASE64=/usr/bin/base64
          "$GIT" -c core.hooksPath=/dev/null config user.name "bot"
          "$GIT" -c core.hooksPath=/dev/null commit -am "msg"
          AUTH_HEADER="AUTHORIZATION: basic $(printf 'x:%s' "$GH_PAT" | "$BASE64" -w0)"
          "$GIT" -c core.hooksPath=/dev/null \
            -c http.https://github.com/.extraheader="$AUTH_HEADER" \
            push origin "HEAD:$PR_HEAD_REF"
EOF
}

@test "passes on a hardened PAT-bearing push step" {
  write_hardened_workflow
  run "$CHECK" --workflow "$TMP_WF/wf.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
  [[ "$output" != *"FAIL"* ]]
}

@test "fails when git is invoked bare (PATH override reachable)" {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  push:
    steps:
      - name: Commit and push
        env:
          GH_PAT: ${{ secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN }}
        run: |
          set -euo pipefail
          git commit -am "msg"
          git push origin HEAD
EOF
  run "$CHECK" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"absolute path"* ]]
}

@test "fails when a \$GIT invocation does not disable repository hooks" {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  push:
    steps:
      - name: Commit and push
        env:
          GH_PAT: ${{ secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN }}
        run: |
          set -euo pipefail
          GIT=/usr/bin/git
          "$GIT" -c core.hooksPath=/dev/null config user.name "bot"
          "$GIT" commit -am "msg"
EOF
  run "$CHECK" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"core.hooksPath=/dev/null"* ]]
}

@test "fails when the PAT-bearing step executes a repository script" {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  push:
    steps:
      - name: Commit and push
        env:
          GH_PAT: ${{ secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN }}
        run: |
          set -euo pipefail
          GIT=/usr/bin/git
          msg="$(./scripts/auto-format.sh --commit-message)"
          "$GIT" -c core.hooksPath=/dev/null commit -am "$msg"
EOF
  run "$CHECK" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"repository script"* ]]
}

@test "fails when base64 is not pinned to an absolute path" {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  push:
    steps:
      - name: Commit and push
        env:
          GH_PAT: ${{ secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN }}
        run: |
          set -euo pipefail
          GIT=/usr/bin/git
          AUTH_HEADER="basic $(printf 'x:%s' "$GH_PAT" | base64 -w0)"
          "$GIT" -c core.hooksPath=/dev/null push origin HEAD:main
EOF
  run "$CHECK" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"pin base64"* ]]
}

@test "fails when base64 is pinned but still invoked bare" {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  push:
    steps:
      - name: Commit and push
        env:
          GH_PAT: ${{ secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN }}
        run: |
          set -euo pipefail
          GIT=/usr/bin/git
          BASE64=/usr/bin/base64
          AUTH_HEADER="basic $(printf 'x:%s' "$GH_PAT" | base64 -w0)"
          "$GIT" -c core.hooksPath=/dev/null push origin HEAD:main
EOF
  run "$CHECK" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"bare 'base64'"* ]]
}

@test "passes when the PAT-bearing step never uses base64" {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  push:
    steps:
      - name: Commit and push
        env:
          GH_PAT: ${{ secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN }}
        run: |
          set -euo pipefail
          GIT=/usr/bin/git
          "$GIT" -c core.hooksPath=/dev/null push origin HEAD:main
EOF
  run "$CHECK" --workflow "$TMP_WF/wf.yml"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

@test "fails when no step binds GH_PAT to ACTIONS_PUSH" {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  push:
    steps:
      - name: Push
        run: git push origin HEAD
EOF
  run "$CHECK" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"ACTIONS_PUSH"* ]]
}

@test "fails when the PAT-bearing step has no literal run block" {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
jobs:
  push:
    steps:
      - name: Push
        env:
          GH_PAT: ${{ secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN }}
        uses: some/action@v1
EOF
  run "$CHECK" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"run"* ]]
}

@test "reports a missing workflow file as an error" {
  run "$CHECK" --workflow "$TMP_WF/absent.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "shipped auto-format and version-increment workflows validate cleanly" {
  run "$CHECK"
  [ "$status" -eq 0 ]
  [[ "$output" == *"auto-format.yml"* ]]
  [[ "$output" == *"version-increment.yml"* ]]
  [[ "$output" != *"FAIL"* ]]
}
