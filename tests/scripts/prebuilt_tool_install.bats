#!/usr/bin/env bats
# Tests for scripts/check-prebuilt-tool-install.sh — Issue #208.
#
# Verifies that the validator distinguishes a prebuilt-binary install
# (taiki-e/install-action) from a source compile (cargo install <tool>) using
# synthetic workflow YAML, so the real workflows can change without coupling
# the unit tests to their exact contents.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-prebuilt-tool-install.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# A workflow that installs the tool from a prebuilt binary (the desired state).
write_prebuilt_workflow() {
  local file="$1"
  local tool="$2"
  cat >"$file" <<EOF
name: Example
on: [push]
jobs:
  job:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@93cb6efe18208431cddfb8368fd83d5badbf9bfd  # v5
      - uses: dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8  # stable
      - uses: taiki-e/install-action@7a79fe8c3a13344501c80d99cae481c1c9085912  # v2.81.10
        with:
          tool: ${tool}
EOF
}

# A workflow that compiles the tool from source (the regression to reject).
write_source_workflow() {
  local file="$1"
  local tool="$2"
  cat >"$file" <<EOF
name: Example
on: [push]
jobs:
  job:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@93cb6efe18208431cddfb8368fd83d5badbf9bfd  # v5
      - uses: dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8  # stable
      - run: cargo install ${tool} --locked
EOF
}

@test "passes when the tool is installed via taiki-e/install-action" {
  write_prebuilt_workflow "$TMP_WF/wf.yml" "cargo-audit"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/wf.yml" --tool cargo-audit
  [ "$status" -eq 0 ]
  # Issue #360: prove every rule was individually evaluated and passed via the
  # machine-checkable "OK   " marker rather than pinning informational wording.
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 2 ]
}

@test "fails when the tool is compiled from source via cargo install" {
  write_source_workflow "$TMP_WF/wf.yml" "cargo-cyclonedx"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/wf.yml" --tool cargo-cyclonedx
  [ "$status" -ne 0 ]
  [[ "$output" == *"compiles cargo-cyclonedx from source"* ]]
}

@test "fails when no prebuilt install step is present" {
  cat >"$TMP_WF/wf.yml" <<'EOF'
name: Example
on: [push]
jobs:
  job:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@93cb6efe18208431cddfb8368fd83d5badbf9bfd  # v5
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/wf.yml" --tool cargo-deny
  [ "$status" -ne 0 ]
  [[ "$output" == *"no prebuilt taiki-e/install-action step for cargo-deny"* ]]
}

@test "reports a missing workflow file" {
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/nope.yml" --tool cargo-audit
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "requires --tool alongside --workflow" {
  write_prebuilt_workflow "$TMP_WF/wf.yml" "cargo-audit"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/wf.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"must be supplied together"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "validates a directory of canonical workflows" {
  mkdir -p "$TMP_WF/wf"
  write_prebuilt_workflow "$TMP_WF/wf/cargo-audit.yml" "cargo-audit"
  write_prebuilt_workflow "$TMP_WF/wf/security.yml" "cargo-audit"
  write_prebuilt_workflow "$TMP_WF/wf/sbom.yml" "cargo-cyclonedx"
  write_prebuilt_workflow "$TMP_WF/wf/ci.yml" "cargo-deny"
  run "$SCRIPT_UNDER_TEST" --workflows "$TMP_WF/wf"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

@test "real repository workflows install tools from prebuilt binaries" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
