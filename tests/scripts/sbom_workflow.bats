#!/usr/bin/env bats
# Tests for scripts/check-sbom-workflow.sh — Issue #172.
#
# Exercises the SBOM workflow validator with synthetic workflow YAML in
# temporary directories so behaviour (exit codes, reported failures) is
# verified end-to-end without mutating the real workflow file.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-sbom-workflow.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# Canonical hardened workflow. Failure tests mutate this fixture to drop or
# break one rule at a time.
write_sbom_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: SBOM

# Generate a CycloneDX SBOM for the rust_scorer binary (Issue #172).

on:
  pull_request:
    branches: ["*"]
  push:
    branches: [Develop]
  workflow_dispatch:

permissions:
  contents: read

jobs:
  sbom:
    runs-on: ubuntu-latest
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@v5
      - uses: actions/checkout@v5
        with:
          repository: stSoftwareAU/NEAT-AI-core
          ref: Develop
          path: NEAT-AI-core
      - name: Link NEAT-AI-core sibling path expected by Cargo
        run: ln -s a b
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install cargo-cyclonedx --locked
      - run: cargo cyclonedx --format json --all
      - uses: actions/upload-artifact@v7
        with:
          name: sbom
          path: '**/*.cdx.json'
EOF
}

@test "passes on the canonical fixture" {
  write_sbom_workflow "$TMP_WF/sbom.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/sbom.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"triggers on a build event"* ]]
  [[ "$output" == *"permissions block grants only contents: read"* ]]
  [[ "$output" == *"actions/checkout pinned"* ]]
  [[ "$output" == *"dtolnay/rust-toolchain present"* ]]
  [[ "$output" == *"CycloneDX SBOM generated"* ]]
  [[ "$output" == *"SBOM uploaded as an artefact"* ]]
}

@test "fails when no build trigger is present" {
  write_sbom_workflow "$TMP_WF/sbom.yml"
  python3 - "$TMP_WF/sbom.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("  pull_request:\n    branches: [\"*\"]\n", "")
text = text.replace("  push:\n    branches: [Develop]\n", "")
text = text.replace("  workflow_dispatch:\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/sbom.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no build trigger"* ]]
}

@test "fails when the permissions block is missing" {
  write_sbom_workflow "$TMP_WF/sbom.yml"
  python3 - "$TMP_WF/sbom.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("permissions:\n  contents: read\n\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/sbom.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no 'permissions: contents: read'"* ]]
}

@test "fails when actions/checkout is unpinned" {
  write_sbom_workflow "$TMP_WF/sbom.yml"
  sed -i.bak 's|actions/checkout@v5|actions/checkout@main|' "$TMP_WF/sbom.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/sbom.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/checkout"* ]]
  [[ "$output" == *"not pinned"* ]]
}

@test "fails when dtolnay/rust-toolchain is missing" {
  write_sbom_workflow "$TMP_WF/sbom.yml"
  sed -i.bak '/dtolnay\/rust-toolchain/d' "$TMP_WF/sbom.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/sbom.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"dtolnay/rust-toolchain"* ]]
  [[ "$output" == *"missing"* ]]
}

@test "fails when cargo cyclonedx is not invoked" {
  write_sbom_workflow "$TMP_WF/sbom.yml"
  sed -i.bak '/cyclonedx/d' "$TMP_WF/sbom.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/sbom.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"CycloneDX SBOM"* ]]
  [[ "$output" == *"not generated"* ]]
}

@test "fails when the SBOM is not uploaded as an artefact" {
  write_sbom_workflow "$TMP_WF/sbom.yml"
  python3 - "$TMP_WF/sbom.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace(
    "      - uses: actions/upload-artifact@v7\n"
    "        with:\n"
    "          name: sbom\n"
    "          path: '**/*.cdx.json'\n",
    "",
)
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/sbom.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not uploaded"* ]]
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

@test "real repository SBOM workflow satisfies every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
