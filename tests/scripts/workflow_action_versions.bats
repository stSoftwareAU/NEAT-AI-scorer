#!/usr/bin/env bats
# Tests for scripts/check-workflow-action-versions.sh — Issues #24 and #100.
#
# Drives the validator with synthetic workflow YAML in a temp directory so
# policy behaviour (exit codes, reported failures) is verified end-to-end
# without relying on the real repo workflows staying static.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-workflow-action-versions.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# Forty-character placeholder SHAs used by the synthetic fixtures below.
# The validator only checks the SHA shape (40 lowercase hex chars), so any
# distinct hex string is acceptable for the unit tests.
SHA_CHECKOUT="1111111111111111111111111111111111111111"
SHA_CACHE="2222222222222222222222222222222222222222"
SHA_TOOLCHAIN="3333333333333333333333333333333333333333"
SHA_SHELLCHECK="4444444444444444444444444444444444444444"
SHA_PR="5555555555555555555555555555555555555555"
SHA_DEPREV="6666666666666666666666666666666666666666"
SHA_AUDIT="7777777777777777777777777777777777777777"
SHA_SETUP_NODE="8888888888888888888888888888888888888888"

# A minimal compliant workflow — every action SHA-pinned with the version
# label in the trailing comment. Individual failure tests rewrite a single
# line to break exactly one rule at a time.
write_compliant_workflow() {
  local file="$1"
  cat >"$file" <<EOF
name: Example
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@${SHA_CHECKOUT}  # v5
      - uses: actions/cache@${SHA_CACHE}  # v5
      - uses: actions/setup-node@${SHA_SETUP_NODE}  # v6
      - uses: dtolnay/rust-toolchain@${SHA_TOOLCHAIN}  # stable, frozen 2026-05-18
      - uses: ludeeus/action-shellcheck@${SHA_SHELLCHECK}  # v2.0.0
      - uses: peter-evans/create-pull-request@${SHA_PR}  # v8
      - uses: actions/dependency-review-action@${SHA_DEPREV}  # v4.9.0
      - uses: rustsec/audit-check@${SHA_AUDIT}  # v2.0.0
EOF
}

@test "passes on a SHA-pinned workflow that satisfies every policy rule" {
  write_compliant_workflow "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"actions/checkout@${SHA_CHECKOUT}"* ]]
  [[ "$output" == *"SHA-pinned"* ]]
  [[ "$output" == *"Node 20 exception, tracked"* ]]
}

@test "fails when an action is pinned to a version tag instead of a SHA" {
  cat >"$TMP_WF/example.yml" <<'EOF'
name: Example
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
EOF
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/checkout@v5"* ]]
  [[ "$output" == *"not SHA-pinned"* ]]
}

@test "fails when an action is pinned to a branch ref instead of a SHA" {
  cat >"$TMP_WF/example.yml" <<'EOF'
name: Example
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: dtolnay/rust-toolchain@stable
EOF
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"dtolnay/rust-toolchain@stable"* ]]
  [[ "$output" == *"not SHA-pinned"* ]]
}

@test "fails when a SHA-pinned action has no trailing version comment" {
  cat >"$TMP_WF/example.yml" <<EOF
name: Example
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@${SHA_CHECKOUT}
EOF
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"missing trailing"* ]]
}

@test "fails when actions/checkout version comment is older than v5" {
  write_compliant_workflow "$TMP_WF/example.yml"
  sed -i.bak "s|actions/checkout@${SHA_CHECKOUT}  # v5|actions/checkout@${SHA_CHECKOUT}  # v4|" "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"requires v5 or newer"* ]]
}

@test "fails when actions/cache version comment is older than v5" {
  write_compliant_workflow "$TMP_WF/example.yml"
  sed -i.bak "s|actions/cache@${SHA_CACHE}  # v5|actions/cache@${SHA_CACHE}  # v4|" "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"requires v5 or newer"* ]]
}

@test "fails when actions/setup-node version comment is older than v6 (Node 20 — Issue #137)" {
  write_compliant_workflow "$TMP_WF/example.yml"
  # setup-node v4 and v5 still ship a Node 20 runtime; v6 is the first
  # Node 24 release. Regressing to v4 or v5 re-introduces the Node.js 20
  # deprecation warning that triggered Issue #137.
  sed -i.bak "s|actions/setup-node@${SHA_SETUP_NODE}  # v6|actions/setup-node@${SHA_SETUP_NODE}  # v4|" "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"requires v6 or newer"* ]]
}

@test "fails when actions/setup-node is pinned to v5 (still Node 20 — Issue #137)" {
  write_compliant_workflow "$TMP_WF/example.yml"
  sed -i.bak "s|actions/setup-node@${SHA_SETUP_NODE}  # v6|actions/setup-node@${SHA_SETUP_NODE}  # v5|" "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"requires v6 or newer"* ]]
}

@test "fails when peter-evans/create-pull-request comment is older than v8" {
  write_compliant_workflow "$TMP_WF/example.yml"
  sed -i.bak "s|peter-evans/create-pull-request@${SHA_PR}  # v8|peter-evans/create-pull-request@${SHA_PR}  # v7|" "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"requires v8 or newer"* ]]
}

@test "fails when a Node 20 exception is bumped to an unknown major" {
  write_compliant_workflow "$TMP_WF/example.yml"
  # rustsec/audit-check has no v3 yet — enforcing "stay on v2" surfaces the
  # accidental bump so someone can verify the new major before we adopt it.
  sed -i.bak "s|rustsec/audit-check@${SHA_AUDIT}  # v2.0.0|rustsec/audit-check@${SHA_AUDIT}  # v3.0.0|" "$TMP_WF/example.yml"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"tracked Node 20 exception"* ]]
}

@test "warns but does not fail on an unknown SHA-pinned action" {
  cat >"$TMP_WF/example.yml" <<EOF
name: Example
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: some-random/action@${SHA_CHECKOUT}  # v1
EOF
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" == *"WARN"* ]]
  [[ "$output" == *"some-random/action"* ]]
  [[ "$output" == *"not in policy tables"* ]]
}

@test "ignores uses lines that live inside YAML comments" {
  cat >"$TMP_WF/example.yml" <<EOF
name: Example
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      # Historical note: we used to rely on \`uses: actions/checkout@v4\`.
      - uses: actions/checkout@${SHA_CHECKOUT}  # v5
EOF
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -eq 0 ]
  [[ "$output" != *"actions/checkout@v4"* ]]
  [[ "$output" == *"actions/checkout@${SHA_CHECKOUT}"* ]]
}

@test "ignores reusable-workflow calls (./.github/workflows/*.yml)" {
  cat >"$TMP_WF/example.yml" <<'EOF'
name: Example
on: [pull_request]
jobs:
  security:
    uses: ./.github/workflows/security.yml
EOF
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  # No actions to audit, but the scan still runs — exits 0 with no findings.
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
  [[ "$output" != *"WARN"* ]]
}

@test "reports an error when the workflows directory does not exist" {
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF/does-not-exist"
  [ "$status" -ne 0 ]
  [[ "$output" == *"Workflows directory not found"* ]]
}

@test "reports an error when the workflows directory is empty" {
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF"
  [ "$status" -ne 0 ]
  [[ "$output" == *"No workflow files found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "real repository workflows satisfy the Node 24 compat policy" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
