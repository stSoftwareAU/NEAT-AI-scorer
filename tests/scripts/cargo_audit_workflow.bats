#!/usr/bin/env bats
# Tests for scripts/check-cargo-audit-workflow.sh — Issue #64.
#
# Exercises the Cargo Security Audit workflow validator with synthetic
# workflow YAML in temporary directories so behaviour (exit codes, reported
# failures) is verified end-to-end without mutating the real workflow file.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-cargo-audit-workflow.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# Canonical hardened workflow. Failure tests mutate this fixture to drop or
# break one rule at a time.
write_audit_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Cargo Audit

# Standalone Cargo Security Audit workflow (Issue #64). Mirrors the
# cargo-audit step embedded in the reusable security workflow but adds a
# weekly schedule so advisories published after the last PR still surface.

on:
  pull_request:
    branches: ["*"]
  schedule:
    - cron: "0 6 * * 1"
  workflow_dispatch:

permissions:
  contents: read

jobs:
  audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install cargo-audit --locked
      - run: cargo audit
EOF
}

@test "passes on the canonical fixture" {
  write_audit_workflow "$TMP_WF/cargo-audit.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-audit.yml"
  [ "$status" -eq 0 ]
  # Issue #360: prove every rule was individually evaluated and passed via the
  # machine-checkable "OK   " marker rather than pinning informational wording.
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 6 ]
}

@test "fails when the workflow is not triggered on pull_request" {
  write_audit_workflow "$TMP_WF/cargo-audit.yml"
  python3 - "$TMP_WF/cargo-audit.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("  pull_request:\n    branches: [\"*\"]\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-audit.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not triggered on pull_request"* ]]
}

@test "fails when the workflow has no schedule trigger" {
  write_audit_workflow "$TMP_WF/cargo-audit.yml"
  python3 - "$TMP_WF/cargo-audit.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("  schedule:\n    - cron: \"0 6 * * 1\"\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-audit.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no schedule"* ]]
}

@test "fails when the permissions block is missing" {
  write_audit_workflow "$TMP_WF/cargo-audit.yml"
  python3 - "$TMP_WF/cargo-audit.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("permissions:\n  contents: read\n\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-audit.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no 'permissions: contents: read'"* ]]
}

@test "fails when actions/checkout is unpinned" {
  write_audit_workflow "$TMP_WF/cargo-audit.yml"
  sed -i.bak 's|actions/checkout@v5|actions/checkout@main|' "$TMP_WF/cargo-audit.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-audit.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/checkout"* ]]
  [[ "$output" == *"not pinned"* ]]
}

@test "fails when dtolnay/rust-toolchain is missing" {
  write_audit_workflow "$TMP_WF/cargo-audit.yml"
  sed -i.bak '/dtolnay\/rust-toolchain/d' "$TMP_WF/cargo-audit.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-audit.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"dtolnay/rust-toolchain"* ]]
  [[ "$output" == *"missing"* ]]
}

@test "fails when cargo audit is not invoked" {
  write_audit_workflow "$TMP_WF/cargo-audit.yml"
  sed -i.bak '/cargo audit/d' "$TMP_WF/cargo-audit.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-audit.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"cargo audit"* ]]
  [[ "$output" == *"not invoked"* ]]
}

@test "accepts rustsec/audit-check action as the audit entry point" {
  cat >"$TMP_WF/cargo-audit.yml" <<'EOF'
name: Cargo Audit
# Issue #64 — uses the official rustsec/audit-check action.
on:
  pull_request:
    branches: ["*"]
  schedule:
    - cron: "0 6 * * 1"
permissions:
  contents: read
jobs:
  audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
      - uses: rustsec/audit-check@v2
        with:
          token: ${{ secrets.GITHUB_TOKEN }}
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-audit.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"cargo audit invoked"* ]]
}

@test "reports an error when the workflow file does not exist" {
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/does-not-exist.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "real repository cargo-audit workflow satisfies every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
