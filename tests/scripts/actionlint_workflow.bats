#!/usr/bin/env bats
# Tests for scripts/check-actionlint-workflow.sh — Issue #195.
#
# Exercises the standalone Actionlint workflow validator with synthetic
# workflow YAML in temporary directories so behaviour (exit codes, reported
# failures) is verified end-to-end without mutating the real workflow file.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-actionlint-workflow.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# Canonical hardened workflow. Failure tests mutate this fixture to drop or
# break one rule at a time.
write_actionlint_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Actionlint

on:
  pull_request:
    branches: ["*", "milestone/**"]
  push:
    branches: [main, master, Develop]

permissions:
  contents: read

concurrency:
  group: actionlint-${{ github.ref }}
  cancel-in-progress: true

jobs:
  actionlint:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    env:
      ACTIONLINT_VERSION: "1.7.7"
    steps:
      - uses: actions/checkout@93cb6efe18208431cddfb8368fd83d5badbf9bfd  # v5
      - name: Install actionlint
        run: |
          curl -fsSL -o download-actionlint.bash \
            "https://raw.githubusercontent.com/rhysd/actionlint/v${ACTIONLINT_VERSION}/scripts/download-actionlint.bash"
          bash download-actionlint.bash "${ACTIONLINT_VERSION}"
          echo "${PWD}" >> "${GITHUB_PATH}"
      - name: Run actionlint
        run: actionlint -color
EOF
}

@test "passes on the canonical fixture" {
  write_actionlint_workflow "$TMP_WF/actionlint.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/actionlint.yml"
  [ "$status" -eq 0 ]
  # Issue #360: prove every rule was individually evaluated and passed via the
  # machine-checkable "OK   " marker rather than pinning informational wording.
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 6 ]
}

@test "fails when the workflow is not triggered on pull_request" {
  write_actionlint_workflow "$TMP_WF/actionlint.yml"
  python3 - "$TMP_WF/actionlint.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("  pull_request:\n    branches: [\"*\", \"milestone/**\"]\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/actionlint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not triggered on pull_request"* ]]
}

@test "fails when the permissions block is missing" {
  write_actionlint_workflow "$TMP_WF/actionlint.yml"
  python3 - "$TMP_WF/actionlint.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("permissions:\n  contents: read\n\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/actionlint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no 'permissions: contents: read'"* ]]
}

@test "fails when actions/checkout is unpinned" {
  write_actionlint_workflow "$TMP_WF/actionlint.yml"
  sed -i.bak 's|actions/checkout@93cb6efe18208431cddfb8368fd83d5badbf9bfd  # v5|actions/checkout@main|' "$TMP_WF/actionlint.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/actionlint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/checkout"* ]]
  [[ "$output" == *"not pinned"* ]]
}

@test "fails when actions/checkout step is missing" {
  write_actionlint_workflow "$TMP_WF/actionlint.yml"
  sed -i.bak '/actions\/checkout/d' "$TMP_WF/actionlint.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/actionlint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/checkout"* ]]
  [[ "$output" == *"missing"* ]]
}

@test "fails when the actionlint install step is missing" {
  write_actionlint_workflow "$TMP_WF/actionlint.yml"
  sed -i.bak '/download-actionlint\.bash/d' "$TMP_WF/actionlint.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/actionlint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"install step missing"* ]]
}

@test "fails when actionlint is not invoked" {
  write_actionlint_workflow "$TMP_WF/actionlint.yml"
  python3 - "$TMP_WF/actionlint.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("      - name: Run actionlint\n        run: actionlint -color\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/actionlint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not invoked"* ]]
}

@test "fails when the pull_request branch filter omits milestone branches" {
  write_actionlint_workflow "$TMP_WF/actionlint.yml"
  # Revert to the pre-fix bare "*" filter, which does not match milestone/<slug>.
  sed -i.bak 's|branches: \["\*", "milestone/\*\*"\]|branches: ["*"]|' "$TMP_WF/actionlint.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/actionlint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"milestone"* ]]
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

@test "real repository actionlint workflow satisfies every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
