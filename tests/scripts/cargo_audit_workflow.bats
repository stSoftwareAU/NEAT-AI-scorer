#!/usr/bin/env bats
# Tests for scripts/check-cargo-audit-workflow.sh — Issues #64, #603.
#
# Exercises the Cargo Security Audit workflow validator with synthetic
# workflow YAML in temporary directories so behaviour (exit codes, reported
# failures) is verified end-to-end without mutating the real workflow file.

load 'test_helper'

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
    branches: ["*", "milestone/**"]
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

# The Issue #603 end state: the standalone workflow keeps only its weekly cron
# and the manual trigger, leaving PR-time auditing to a single other workflow.
write_scheduled_only_audit_workflow() {
  local file="$1"
  write_audit_workflow "$file"
  python3 - "$file" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace('  pull_request:\n    branches: ["*", "milestone/**"]\n', "")
with open(path, "w") as fh:
    fh.write(text)
PY
}

# A second PR-time cargo audit reached through a reusable workflow: `ci.yml`
# fires on pull_request and calls `security.yml`, which runs the RustSec action.
write_pr_reachable_audit_pair() {
  local dir="$1"
  cat >"$dir/ci.yml" <<'EOF'
name: CI
on:
  pull_request:
    branches: [Develop, milestone/*]
jobs:
  security:
    uses: ./.github/workflows/security.yml
EOF
  cat >"$dir/security.yml" <<'EOF'
name: Security Reusable Workflow
on:
  workflow_call:
jobs:
  security:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      # Prose mentioning `cargo audit` must not count as an invocation.
      - uses: rustsec/audit-check@v2
EOF
}

@test "passes on the canonical fixture" {
  write_audit_workflow "$TMP_WF/cargo-audit.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-audit.yml"
  [ "$status" -eq 0 ]
  # Issue #360: prove every rule was individually evaluated and passed via the
  # machine-checkable "OK   " marker rather than pinning informational wording.
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 8 ]
  [[ "$output" != *"WARN"* ]]
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
    branches: ["*", "milestone/**"]
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

@test "fails when the pull_request branch filter omits milestone branches" {
  write_audit_workflow "$TMP_WF/cargo-audit.yml"
  # Revert to the pre-fix bare "*" filter, which does not match milestone/<slug>.
  sed -i.bak 's|branches: \["\*", "milestone/\*\*"\]|branches: ["*"]|' "$TMP_WF/cargo-audit.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-audit.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"milestone"* ]]
}

@test "warns when a second PR-reachable workflow audits the same lockfile (issue #603)" {
  write_audit_workflow "$TMP_WF/cargo-audit.yml"
  write_pr_reachable_audit_pair "$TMP_WF"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-audit.yml"
  # A duplicate is a deficiency the validator cannot fail on: fixing it means
  # editing .github/workflows/, which the automation worker cannot push.
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
  [[ "$output" == *"WARN"* ]]
  [[ "$output" == *"Issue #603"* ]]
  [[ "$output" == *"cargo-audit.yml"* ]]
  [[ "$output" == *"security.yml"* ]]
}

@test "passes without a pull_request trigger once another workflow owns the PR audit (issue #603)" {
  write_scheduled_only_audit_workflow "$TMP_WF/cargo-audit.yml"
  write_pr_reachable_audit_pair "$TMP_WF"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-audit.yml"
  [ "$status" -eq 0 ]
  [[ "$output" != *"WARN"* ]]
  [[ "$output" == *"no pull_request trigger"* ]]
  # The milestone branch-filter rule is pull_request-only, so it is not
  # evaluated — and must not fail — on a schedule-only workflow.
  [[ "$output" != *"milestone/* — milestone PRs skip the gate"* ]]
}

@test "fails when no workflow provides a PR-time cargo audit (issue #603)" {
  write_scheduled_only_audit_workflow "$TMP_WF/cargo-audit.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-audit.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no workflow runs a cargo audit on pull_request"* ]]
}

@test "prose mentioning cargo audit does not count as a second invocation (issue #603)" {
  write_audit_workflow "$TMP_WF/cargo-audit.yml"
  cat >"$TMP_WF/lint.yml" <<'EOF'
name: Lint
# This workflow does not run `cargo audit` — cargo-audit.yml owns that.
on:
  pull_request:
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - run: cargo fmt --check
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-audit.yml"
  [ "$status" -eq 0 ]
  [[ "$output" != *"WARN"* ]]
}

@test "reports an error when the workflow file does not exist" {
  assert_missing_target_rejected "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/does-not-exist.yml"
}

@test "reports an error when the workflows directory does not exist" {
  write_audit_workflow "$TMP_WF/cargo-audit.yml"
  assert_missing_target_rejected "$SCRIPT_UNDER_TEST" \
    --workflow "$TMP_WF/cargo-audit.yml" --workflows "$TMP_WF/no-such-dir"
}

@test "unknown flag prints usage and exits non-zero" {
  assert_unknown_flag_rejected "$SCRIPT_UNDER_TEST"
}

@test "real repository cargo-audit workflow satisfies every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
