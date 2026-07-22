#!/usr/bin/env bats
# Tests for scripts/check-markdown-lint-workflow.sh — Issue #63.
#
# Exercises the standalone Markdown Lint workflow validator with synthetic
# workflow YAML in temporary directories so behaviour (exit codes, reported
# failures) is verified end-to-end without mutating the real workflow file.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-markdown-lint-workflow.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# Canonical hardened workflow. Failure tests mutate this fixture to drop or
# break one rule at a time.
#
# Issue #371: a lint/checker workflow must gate the PR only — it must NOT
# trigger on push to the default branch `Develop`. The canonical fixture
# therefore carries no `push:` trigger (this reverses the Issue #207 policy
# that previously required push→Develop).
write_markdown_lint_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Markdown Lint

on:
  pull_request:
    branches: ["*"]

permissions:
  contents: read

jobs:
  markdownlint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: actions/setup-node@v4
        with:
          node-version: "lts/*"
      - name: Install markdownlint-cli2
        run: npm install -g markdownlint-cli2
      - name: Run markdownlint-cli2
        run: markdownlint-cli2
EOF
}

@test "passes on the canonical fixture" {
  write_markdown_lint_workflow "$TMP_WF/markdown-lint.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/markdown-lint.yml"
  [ "$status" -eq 0 ]
  # Issue #360: prove every rule was individually evaluated and passed via the
  # machine-checkable "OK   " marker rather than pinning informational wording.
  [ "$(grep -c '^OK   ' <<<"$output")" -eq 7 ]
}

# Business-logic change (Issue #371 reverses Issue #207): a lint/checker
# workflow must gate the PR only, so a `push:` trigger reaching the default
# branch `Develop` is now a failure rather than a requirement.
@test "fails when the push trigger targets the default branch Develop (Issue #371)" {
  write_markdown_lint_workflow "$TMP_WF/markdown-lint.yml"
  # Re-introduce a push→Develop trigger to prove the checker rejects it.
  python3 - "$TMP_WF/markdown-lint.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace(
    "on:\n  pull_request:\n    branches: [\"*\"]\n",
    "on:\n  pull_request:\n    branches: [\"*\"]\n  push:\n    branches: [main, master, Develop]\n",
)
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/markdown-lint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"push trigger targets the default branch (Develop)"* ]]
}

# A push trigger that excludes the default branch is acceptable — the workflow
# may still lint pushes to non-default branches without duplicating the PR gate.
@test "passes when a push trigger excludes the default branch Develop (Issue #371)" {
  write_markdown_lint_workflow "$TMP_WF/markdown-lint.yml"
  python3 - "$TMP_WF/markdown-lint.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace(
    "on:\n  pull_request:\n    branches: [\"*\"]\n",
    "on:\n  pull_request:\n    branches: [\"*\"]\n  push:\n    branches: [main, master]\n",
)
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/markdown-lint.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"push trigger excludes the default branch (Develop)"* ]]
}

# An unfiltered push trigger (no branches list) reaches every branch, including
# the default — the checker must reject it.
@test "fails when the push trigger has no branches filter (Issue #371)" {
  write_markdown_lint_workflow "$TMP_WF/markdown-lint.yml"
  python3 - "$TMP_WF/markdown-lint.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace(
    "on:\n  pull_request:\n    branches: [\"*\"]\n",
    "on:\n  pull_request:\n    branches: [\"*\"]\n  push:\n",
)
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/markdown-lint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"unfiltered branches list"* ]]
}

@test "fails when the workflow is not triggered on pull_request" {
  write_markdown_lint_workflow "$TMP_WF/markdown-lint.yml"
  python3 - "$TMP_WF/markdown-lint.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("  pull_request:\n    branches: [\"*\"]\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/markdown-lint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not triggered on pull_request"* ]]
}

@test "fails when the permissions block is missing" {
  write_markdown_lint_workflow "$TMP_WF/markdown-lint.yml"
  python3 - "$TMP_WF/markdown-lint.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("permissions:\n  contents: read\n\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/markdown-lint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no 'permissions: contents: read'"* ]]
}

@test "fails when actions/checkout is unpinned" {
  write_markdown_lint_workflow "$TMP_WF/markdown-lint.yml"
  sed -i.bak 's|actions/checkout@v5|actions/checkout@main|' "$TMP_WF/markdown-lint.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/markdown-lint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/checkout"* ]]
  [[ "$output" == *"not pinned"* ]]
}

@test "fails when actions/setup-node is unpinned" {
  write_markdown_lint_workflow "$TMP_WF/markdown-lint.yml"
  sed -i.bak 's|actions/setup-node@v4|actions/setup-node@main|' "$TMP_WF/markdown-lint.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/markdown-lint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/setup-node"* ]]
  [[ "$output" == *"not pinned"* ]]
}

@test "fails when actions/setup-node step is missing" {
  write_markdown_lint_workflow "$TMP_WF/markdown-lint.yml"
  sed -i.bak '/actions\/setup-node/d' "$TMP_WF/markdown-lint.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/markdown-lint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/setup-node"* ]]
  [[ "$output" == *"missing"* ]]
}

@test "fails when markdownlint-cli2 install step is missing" {
  write_markdown_lint_workflow "$TMP_WF/markdown-lint.yml"
  sed -i.bak '/npm install -g markdownlint-cli2/d' "$TMP_WF/markdown-lint.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/markdown-lint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"install step missing"* ]]
}

@test "fails when markdownlint-cli2 is not invoked" {
  write_markdown_lint_workflow "$TMP_WF/markdown-lint.yml"
  python3 - "$TMP_WF/markdown-lint.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace("      - name: Run markdownlint-cli2\n        run: markdownlint-cli2\n", "")
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/markdown-lint.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not invoked"* ]]
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

@test "real repository markdown-lint workflow satisfies every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
