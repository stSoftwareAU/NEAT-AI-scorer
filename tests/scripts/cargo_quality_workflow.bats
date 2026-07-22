#!/usr/bin/env bats
# Tests for scripts/check-cargo-quality-workflow.sh — Issue #66.
#
# Exercises the Cargo Quality (fmt + clippy) workflow validator with synthetic
# workflow YAML in temporary directories so behaviour (exit codes, reported
# failures) is verified end-to-end without mutating the real workflow file.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-cargo-quality-workflow.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# Canonical hardened workflow. Failure tests mutate this fixture to drop or
# break one rule at a time.
write_quality_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Cargo Quality

on:
  pull_request:
    branches: ["**"]

permissions:
  contents: read

jobs:
  quality:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - run: cargo fmt --all -- --check
      - run: cargo clippy --all-targets --all-features -- -D warnings
EOF
}

@test "passes on the canonical fixture" {
  write_quality_workflow "$TMP_WF/cargo-quality.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-quality.yml"
  [ "$status" -eq 0 ]
  # Issue #360: prove every rule was individually evaluated and passed via the
  # machine-checkable "OK   " marker rather than pinning informational wording.
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 8 ]
}

@test "fails when the workflow is not triggered on pull_request" {
  write_quality_workflow "$TMP_WF/cargo-quality.yml"
  python3 - "$TMP_WF/cargo-quality.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("  pull_request:\n    branches: [\"**\"]\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-quality.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not triggered on pull_request"* ]]
}

@test "fails when the permissions block is missing" {
  write_quality_workflow "$TMP_WF/cargo-quality.yml"
  python3 - "$TMP_WF/cargo-quality.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("permissions:\n  contents: read\n\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-quality.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no 'permissions: contents: read'"* ]]
}

@test "fails when actions/checkout is unpinned" {
  write_quality_workflow "$TMP_WF/cargo-quality.yml"
  sed -i.bak 's|actions/checkout@v5|actions/checkout@main|' "$TMP_WF/cargo-quality.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-quality.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/checkout"* ]]
  [[ "$output" == *"not pinned"* ]]
}

@test "fails when dtolnay/rust-toolchain is missing" {
  write_quality_workflow "$TMP_WF/cargo-quality.yml"
  sed -i.bak '/dtolnay\/rust-toolchain/d' "$TMP_WF/cargo-quality.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-quality.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"dtolnay/rust-toolchain"* ]]
  [[ "$output" == *"missing"* ]]
}

@test "fails when rustfmt component is not requested" {
  write_quality_workflow "$TMP_WF/cargo-quality.yml"
  sed -i.bak 's|rustfmt, clippy|clippy|' "$TMP_WF/cargo-quality.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-quality.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"rustfmt and clippy"* ]]
}

@test "fails when cargo fmt --check is not invoked" {
  write_quality_workflow "$TMP_WF/cargo-quality.yml"
  sed -i.bak '/cargo fmt/d' "$TMP_WF/cargo-quality.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-quality.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"cargo fmt --check"* ]]
  [[ "$output" == *"not invoked"* ]]
}

@test "fails when cargo clippy is not invoked with -D warnings" {
  write_quality_workflow "$TMP_WF/cargo-quality.yml"
  sed -i.bak 's|-- -D warnings||' "$TMP_WF/cargo-quality.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-quality.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"cargo clippy"* ]]
  [[ "$output" == *"-D warnings"* ]]
}

@test "fails when the branch filter skips milestone/<slug> PRs (Issue #392)" {
  # A `["*"]` filter looks like "any branch" but GitHub's `*` glob does not
  # cross `/`, so milestone/<slug> PRs are silently skipped. The gate must
  # reject it.
  write_quality_workflow "$TMP_WF/cargo-quality.yml"
  sed -i.bak 's|branches: \["\*\*"\]|branches: ["*"]|' "$TMP_WF/cargo-quality.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-quality.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"does not match milestone/<slug>"* ]]
}

@test "passes when the branch filter uses an explicit milestone/* glob" {
  write_quality_workflow "$TMP_WF/cargo-quality.yml"
  sed -i.bak 's|branches: \["\*\*"\]|branches: [Develop, main, milestone/*]|' "$TMP_WF/cargo-quality.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/cargo-quality.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"matches milestone/<slug>"* ]]
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

@test "real repository cargo-quality workflow satisfies every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
