#!/usr/bin/env bats
# Tests for scripts/check-workflow-paths.sh — Issue #18.
#
# Exercises the validator with synthetic workflow YAML in temporary
# directories so behaviour (exit codes, reported failures) is verified
# end-to-end without touching the real .github/workflows tree.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-workflow-paths.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

write_good_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Good
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout code
        uses: actions/checkout@v4
      - name: Checkout NEAT-AI-core (path dependency for neat-core)
        uses: actions/checkout@v4
        with:
          repository: stSoftwareAU/NEAT-AI-core
          ref: Develop
          path: NEAT-AI-core
      - name: Link NEAT-AI-core sibling path expected by Cargo
        run: |
          ln -s "$GITHUB_WORKSPACE/NEAT-AI-core" "$GITHUB_WORKSPACE/../NEAT-AI-core"
EOF
}

@test "passes when every workflow uses an in-workspace path and a sibling link step" {
  write_good_workflow "$TMP_WF/good.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
  [[ "$output" == *"NEAT-AI-core checkout path='NEAT-AI-core'"* ]]
}

@test "fails when a workflow uses a parent-relative checkout path (../NEAT-AI-core)" {
  cat >"$TMP_WF/bad.yml" <<'EOF'
name: Bad
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout NEAT-AI-core (path dependency for neat-core)
        uses: actions/checkout@v4
        with:
          repository: stSoftwareAU/NEAT-AI-core
          ref: Develop
          path: ../NEAT-AI-core
      - name: Link NEAT-AI-core sibling path expected by Cargo
        run: echo link
EOF
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"outside the workspace"* ]]
}

@test "fails when a workflow uses an absolute checkout path" {
  cat >"$TMP_WF/abs.yml" <<'EOF'
name: Absolute
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout NEAT-AI-core (path dependency for neat-core)
        uses: actions/checkout@v4
        with:
          repository: stSoftwareAU/NEAT-AI-core
          ref: Develop
          path: /opt/NEAT-AI-core
      - name: Link NEAT-AI-core sibling path expected by Cargo
        run: echo link
EOF
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"absolute"* ]]
}

@test "fails when a workflow omits the Cargo sibling-link step" {
  cat >"$TMP_WF/missing-link.yml" <<'EOF'
name: NoLink
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout NEAT-AI-core (path dependency for neat-core)
        uses: actions/checkout@v4
        with:
          repository: stSoftwareAU/NEAT-AI-core
          ref: Develop
          path: NEAT-AI-core
EOF
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"Link NEAT-AI-core sibling path"* ]]
}

@test "fails when a NEAT-AI-core checkout has no explicit path at all" {
  cat >"$TMP_WF/nopath.yml" <<'EOF'
name: NoPath
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout NEAT-AI-core (path dependency for neat-core)
        uses: actions/checkout@v4
        with:
          repository: stSoftwareAU/NEAT-AI-core
          ref: Develop
      - name: Link NEAT-AI-core sibling path expected by Cargo
        run: echo link
EOF
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no explicit 'path:'"* ]]
}

@test "ignores workflows that do not check out NEAT-AI-core" {
  cat >"$TMP_WF/unrelated.yml" <<'EOF'
name: Unrelated
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout code
        uses: actions/checkout@v4
EOF
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 0 ]
}

@test "reports an error when the workflows directory does not exist" {
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF/does-not-exist"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "real repository workflows all satisfy the path strategy" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
  # None of the shipped workflows should report FAIL.
  [[ "$output" != *"FAIL"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}
