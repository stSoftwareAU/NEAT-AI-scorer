#!/usr/bin/env bats
# Tests for scripts/check-persist-credentials.sh — Issue #380.
#
# Exercises the `persist-credentials: false` validator end-to-end with synthetic
# workflow YAML in temporary directories, plus one assertion against the real
# repo workflow so the enforced rule and the shipped workflow cannot drift
# apart.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-persist-credentials.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF_DIR="$(mktemp -d)"
  TMP_WF="$TMP_WF_DIR/wf.yml"
  export TMP_WF_DIR TMP_WF
}

teardown() {
  rm -rf "$TMP_WF_DIR"
}

# Canonical fixture: a single-checkout job that correctly hardens its checkout,
# and a two-checkout job that is exempt. Failure tests mutate this one rule at a
# time.
write_good_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: CI
on: [pull_request]

jobs:
  spell-check:
    name: Spell Check
    runs-on: ubuntu-latest
    steps:
      - name: Checkout code
        uses: actions/checkout@93cb6efe18208431cddfb8368fd83d5badbf9bfd  # v5
        with:
          persist-credentials: false
      - run: echo ok

  quality:
    name: Quality Checks
    runs-on: ubuntu-latest
    steps:
      - name: Checkout code
        uses: actions/checkout@93cb6efe18208431cddfb8368fd83d5badbf9bfd  # v5
      - name: Checkout dependency
        uses: actions/checkout@93cb6efe18208431cddfb8368fd83d5badbf9bfd  # v5
        with:
          repository: stSoftwareAU/NEAT-AI-core
      - run: echo ok
EOF
}

@test "passes when a single-checkout job sets persist-credentials: false" {
  write_good_workflow "$TMP_WF"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"job 'spell-check' single checkout sets persist-credentials: false"* ]]
}

@test "exempts a job that runs more than one checkout" {
  write_good_workflow "$TMP_WF"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"job 'quality' runs 2 checkouts"* ]]
}

@test "fails when a single-checkout job omits persist-credentials: false" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [pull_request]

jobs:
  spell-check:
    name: Spell Check
    runs-on: ubuntu-latest
    steps:
      - name: Checkout code
        uses: actions/checkout@93cb6efe18208431cddfb8368fd83d5badbf9bfd  # v5
      - run: echo ok
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"job 'spell-check' single checkout must set 'persist-credentials: false'"* ]]
}

@test "fails when persist-credentials is set to true" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [pull_request]

jobs:
  spell-check:
    name: Spell Check
    runs-on: ubuntu-latest
    steps:
      - name: Checkout code
        uses: actions/checkout@93cb6efe18208431cddfb8368fd83d5badbf9bfd  # v5
        with:
          persist-credentials: true
      - run: echo ok
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"job 'spell-check' single checkout must set 'persist-credentials: false'"* ]]
}

@test "accepts a documented BP-PERSIST-CREDS suppression on a single-checkout job" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [pull_request]

jobs:
  release:
    name: Release
    runs-on: ubuntu-latest
    steps:
      - name: Checkout code
        # best-practice-ignore: BP-PERSIST-CREDS pushes tags back to the repo
        uses: actions/checkout@93cb6efe18208431cddfb8368fd83d5badbf9bfd  # v5
      - run: echo ok
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"documented BP-PERSIST-CREDS suppression"* ]]
}

@test "reports OK for a job with no checkout step" {
  cat >"$TMP_WF" <<'EOF'
name: CI
on: [pull_request]

jobs:
  aggregate:
    name: Aggregate
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"job 'aggregate' has no checkout step"* ]]
}

@test "reports an error when the workflow file does not exist" {
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF_DIR/does-not-exist.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "real repository default workflows satisfy the persist-credentials rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

@test "real repository dependency-review.yml hardens its single checkout (Issue #383)" {
  DR_WF="${BATS_TEST_DIRNAME}/../../.github/workflows/dependency-review.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$DR_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"job 'dependency-review' single checkout sets persist-credentials: false"* ]]
}
