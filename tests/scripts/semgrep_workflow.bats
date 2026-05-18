#!/usr/bin/env bats
# Tests for scripts/check-semgrep-workflow.sh — Issue #47.
#
# Exercises the Semgrep workflow validator with synthetic workflow YAML in
# temporary directories so behaviour (exit codes, reported failures) is
# verified end-to-end without mutating the real workflow file.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-semgrep-workflow.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# Placeholder digest used by the synthetic fixtures below. The validator only
# checks the digest shape (sha256:<64-hex>), so any 64-hex string is fine.
FIXTURE_DIGEST="sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

# Canonical hardened workflow using the container path. Failure tests mutate
# this fixture to drop or break one rule at a time.
write_container_workflow() {
  local file="$1"
  cat >"$file" <<EOF
name: Semgrep

# Semgrep SAST scanning. We use the official semgrep/semgrep container as
# the equivalent of the semgrep/semgrep-action GitHub Action — both consume
# SEMGREP_APP_TOKEN and run \`semgrep ci\`. The container path is upstream's
# recommended PR-scan integration.

on:
  pull_request:
    branches: ["*"]

permissions:
  contents: read

jobs:
  semgrep:
    name: Semgrep SAST Scanning
    runs-on: ubuntu-latest
    container:
      # Semgrep v1.86.0, frozen 2026-05-18.
      image: semgrep/semgrep@${FIXTURE_DIGEST}
    steps:
      - name: Checkout code
        uses: actions/checkout@v5

      - name: Run Semgrep
        run: semgrep ci --config p/default
        env:
          SEMGREP_APP_TOKEN: \${{ secrets.SEMGREP_APP_TOKEN }}
EOF
}

# Alternative hardened workflow that uses semgrep/semgrep-action directly.
write_action_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Semgrep

# Semgrep SAST scanning via semgrep/semgrep-action. The action is pinned by
# major version; bumping it is a deliberate, reviewable change.

on:
  pull_request:
    branches: ["*"]

permissions:
  contents: read

jobs:
  semgrep:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: semgrep/semgrep-action@v1
        env:
          SEMGREP_APP_TOKEN: ${{ secrets.SEMGREP_APP_TOKEN }}
EOF
}

@test "passes on the container fixture" {
  write_container_workflow "$TMP_WF/semgrep.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/semgrep.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"triggers on pull_request"* ]]
  [[ "$output" == *"permissions block grants only contents: read"* ]]
  [[ "$output" == *"Semgrep container image is pinned by digest"* ]]
  [[ "$output" == *"semgrep CLI invocation present"* ]]
  [[ "$output" == *"explicit --config"* ]]
  [[ "$output" == *"SEMGREP_APP_TOKEN sourced from secrets"* ]]
  [[ "$output" == *"rationale comment references semgrep/semgrep-action equivalence"* ]]
}

@test "passes on the semgrep/semgrep-action fixture" {
  write_action_workflow "$TMP_WF/semgrep.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/semgrep.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"semgrep/semgrep-action is pinned"* ]]
  [[ "$output" == *"SEMGREP_APP_TOKEN sourced from secrets"* ]]
}

@test "fails when the workflow is not triggered on pull_request" {
  write_container_workflow "$TMP_WF/semgrep.yml"
  # Replace the 'on:' block with push-only.
  python3 - "$TMP_WF/semgrep.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("on:\n  pull_request:\n    branches: [\"*\"]", "on:\n  push:\n    branches: [main]")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/semgrep.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not triggered on pull_request"* ]]
}

@test "fails when the permissions block is missing" {
  write_container_workflow "$TMP_WF/semgrep.yml"
  python3 - "$TMP_WF/semgrep.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("permissions:\n  contents: read\n\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/semgrep.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no 'permissions: contents: read'"* ]]
}

@test "fails when the container image is unpinned (no digest)" {
  write_container_workflow "$TMP_WF/semgrep.yml"
  sed -i.bak "s|semgrep/semgrep@${FIXTURE_DIGEST}|semgrep/semgrep|" "$TMP_WF/semgrep.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/semgrep.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"container image is not pinned"* ]]
}

@test "fails when the container image is pinned to :latest" {
  write_container_workflow "$TMP_WF/semgrep.yml"
  sed -i.bak "s|semgrep/semgrep@${FIXTURE_DIGEST}|semgrep/semgrep:latest|" "$TMP_WF/semgrep.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/semgrep.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"container image is not pinned"* ]]
}

@test "fails when the container image is pinned to a mutable tag (issue #102)" {
  write_container_workflow "$TMP_WF/semgrep.yml"
  sed -i.bak "s|semgrep/semgrep@${FIXTURE_DIGEST}|semgrep/semgrep:1.86.0|" "$TMP_WF/semgrep.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/semgrep.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"container image is not pinned by digest"* ]]
}

@test "fails when the container image digest is malformed" {
  write_container_workflow "$TMP_WF/semgrep.yml"
  sed -i.bak "s|@${FIXTURE_DIGEST}|@sha256:notavalidhex|" "$TMP_WF/semgrep.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/semgrep.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"container image is not pinned by digest"* ]]
}

@test "fails when no semgrep entry point is present" {
  cat >"$TMP_WF/semgrep.yml" <<'EOF'
name: Semgrep
# semgrep/semgrep-action would be the equivalent.
on:
  pull_request:
    branches: ["*"]
permissions:
  contents: read
jobs:
  semgrep:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - run: echo "no semgrep here"
        env:
          SEMGREP_APP_TOKEN: ${{ secrets.SEMGREP_APP_TOKEN }}
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/semgrep.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no Semgrep entry point"* ]]
}

@test "fails when semgrep/semgrep-action is pinned to a branch ref" {
  write_action_workflow "$TMP_WF/semgrep.yml"
  sed -i.bak 's|semgrep/semgrep-action@v1|semgrep/semgrep-action@main|' "$TMP_WF/semgrep.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/semgrep.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not a numeric vN pin"* ]]
}

@test "fails when --config is missing from the container CLI invocation" {
  write_container_workflow "$TMP_WF/semgrep.yml"
  sed -i.bak 's|semgrep ci --config p/default|semgrep ci|' "$TMP_WF/semgrep.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/semgrep.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no explicit --config"* ]]
}

@test "fails when SEMGREP_APP_TOKEN is not wired from secrets" {
  write_container_workflow "$TMP_WF/semgrep.yml"
  python3 - "$TMP_WF/semgrep.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace(
    "        env:\n          SEMGREP_APP_TOKEN: ${{ secrets.SEMGREP_APP_TOKEN }}\n",
    "",
)
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/semgrep.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"SEMGREP_APP_TOKEN is not wired up"* ]]
}

@test "fails when the semgrep/semgrep-action equivalence comment is missing" {
  write_container_workflow "$TMP_WF/semgrep.yml"
  # Drop every comment line so the rationale block is gone.
  grep -v '^[[:space:]]*#' "$TMP_WF/semgrep.yml" >"$TMP_WF/semgrep.stripped.yml"
  mv "$TMP_WF/semgrep.stripped.yml" "$TMP_WF/semgrep.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/semgrep.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no comment documenting the semgrep/semgrep-action equivalence"* ]]
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

@test "real repository semgrep workflow satisfies every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
