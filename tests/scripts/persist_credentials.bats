#!/usr/bin/env bats
# Tests for scripts/check-persist-credentials.sh — Issue #388.
#
# Exercises the credential-persistence validator end-to-end with synthetic
# workflow YAML in temporary directories, plus one assertion against the real
# repo security.yml so the enforced rule and the shipped workflow cannot drift
# apart.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-persist-credentials.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF_DIR="$(mktemp -d)"
  TMP_WF="$TMP_WF_DIR/security.yml"
  export TMP_WF_DIR TMP_WF
}

teardown() {
  rm -rf "$TMP_WF_DIR"
}

# Canonical fixture: two checkout steps, both disabling credential persistence.
write_good_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Security
on: [workflow_call]

jobs:
  security:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout code
        uses: actions/checkout@abc123  # v5
        with:
          persist-credentials: false

      - name: Checkout sibling
        uses: actions/checkout@abc123  # v5
        with:
          repository: stSoftwareAU/NEAT-AI-core
          path: NEAT-AI-core
          persist-credentials: false

      - name: Run audit
        run: cargo audit
EOF
}

@test "passes when every checkout disables credential persistence" {
  write_good_workflow "$TMP_WF"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -eq 0 ]
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 2 ]
}

@test "fails when a checkout omits persist-credentials: false" {
  cat >"$TMP_WF" <<'EOF'
name: Security
on: [workflow_call]

jobs:
  security:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout code
        uses: actions/checkout@abc123  # v5

      - name: Run audit
        run: cargo audit
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"must set 'persist-credentials: false'"* ]]
}

@test "fails when only one of two checkouts disables persistence" {
  cat >"$TMP_WF" <<'EOF'
name: Security
on: [workflow_call]

jobs:
  security:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout code
        uses: actions/checkout@abc123  # v5
        with:
          persist-credentials: false

      - name: Checkout sibling
        uses: actions/checkout@abc123  # v5
        with:
          repository: stSoftwareAU/NEAT-AI-core
          path: NEAT-AI-core
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 1 ]
  [[ "$output" == *"must set 'persist-credentials: false'"* ]]
}

@test "passes when a checkout carries a documented BP-PERSIST-CREDS ignore" {
  cat >"$TMP_WF" <<'EOF'
name: Security
on: [workflow_call]

jobs:
  security:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout code
        # best-practice-ignore: BP-PERSIST-CREDS — pushes tags back to the repo
        uses: actions/checkout@abc123  # v5
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"documented BP-PERSIST-CREDS ignore"* ]]
}

@test "fails when the workflow has no checkout step to validate" {
  cat >"$TMP_WF" <<'EOF'
name: Security
on: [workflow_call]

jobs:
  security:
    runs-on: ubuntu-latest
    steps:
      - name: Run audit
        run: cargo audit
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no 'actions/checkout' step found"* ]]
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

@test "real repository security.yml disables credential persistence everywhere" {
  run "$SCRIPT_UNDER_TEST" --workflow "${BATS_TEST_DIRNAME}/../../.github/workflows/security.yml"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

@test "real repository semgrep.yml disables credential persistence (Issue #389)" {
  run "$SCRIPT_UNDER_TEST" --workflow "${BATS_TEST_DIRNAME}/../../.github/workflows/semgrep.yml"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

@test "default run validates every guarded workflow (security.yml and semgrep.yml)" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
  [[ "$output" == *"security.yml"* ]]
  [[ "$output" == *"semgrep.yml"* ]]
}
