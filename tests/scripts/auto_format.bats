#!/usr/bin/env bats
# Tests for scripts/auto-format.sh and scripts/check-auto-format-workflow.sh
# (Issue #19 — auto-format PR job).
#
# The helper script is invoked from the workflow to (a) detect whether a
# `cargo fmt --all` produced any changes in the working tree and (b) emit
# a deterministic commit message. Running cargo itself is the workflow's
# job — the script takes the resulting working tree as-is and reports on it,
# which keeps these tests hermetic (no Rust toolchain required).

setup() {
  AUTO_FORMAT="${BATS_TEST_DIRNAME}/../../scripts/auto-format.sh"
  WF_CHECK="${BATS_TEST_DIRNAME}/../../scripts/check-auto-format-workflow.sh"
  [ -x "$AUTO_FORMAT" ] || chmod +x "$AUTO_FORMAT"
  [ -x "$WF_CHECK" ] || chmod +x "$WF_CHECK"

  TMP_REPO="$(mktemp -d)"
  (
    cd "$TMP_REPO"
    git init -q
    git config user.email "test@example.com"
    git config user.name "Test"
    echo "hello" >file.txt
    git add file.txt
    git commit -q -m "initial"
  )
  export TMP_REPO

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_REPO" "$TMP_WF"
}

# --- auto-format.sh --------------------------------------------------------

@test "commit-message prints a deterministic message mentioning rustfmt and issue 19" {
  run "$AUTO_FORMAT" --commit-message
  [ "$status" -eq 0 ]
  first="$output"

  run "$AUTO_FORMAT" --commit-message
  [ "$status" -eq 0 ]
  # Deterministic across invocations.
  [ "$output" = "$first" ]
  [[ "$output" == *"rustfmt"* ]]
  [[ "$output" == *"#19"* ]]
}

@test "check-changes exits 1 on a clean working tree" {
  run "$AUTO_FORMAT" --check-changes --repo "$TMP_REPO"
  [ "$status" -eq 1 ]
  [[ "$output" == *"clean"* ]]
}

@test "check-changes exits 0 when the working tree is dirty" {
  echo "dirty" >>"$TMP_REPO/file.txt"
  run "$AUTO_FORMAT" --check-changes --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
  [[ "$output" == *"dirty"* ]]
}

@test "check-changes exits 0 when a new file appears in the working tree" {
  echo "new" >"$TMP_REPO/new.txt"
  (cd "$TMP_REPO" && git add -N new.txt)
  run "$AUTO_FORMAT" --check-changes --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
}

@test "check-changes exits 1 when only an untracked directory is present" {
  # Regression: NEAT-AI-core/ is checked out alongside the workspace as a
  # path dependency but is not a tracked file. cargo fmt cannot modify it,
  # so its presence must not trigger a commit attempt (issue #22 / PR #34).
  mkdir -p "$TMP_REPO/NEAT-AI-core/src"
  echo "fn main() {}" >"$TMP_REPO/NEAT-AI-core/src/lib.rs"
  run "$AUTO_FORMAT" --check-changes --repo "$TMP_REPO"
  [ "$status" -eq 1 ]
  [[ "$output" == *"clean"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$AUTO_FORMAT" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "missing mode prints usage and exits non-zero" {
  run "$AUTO_FORMAT"
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

# --- check-auto-format-workflow.sh ----------------------------------------

write_good_workflow() {
  cat >"$TMP_WF/auto-format.yml" <<'EOF'
# Auto-format PR job (Issue #19).
name: Auto Format
on:
  pull_request:
    types: [opened, synchronize, reopened]
    branches:
      - Develop
      - "milestone/**"
permissions:
  contents: write
  pull-requests: read
jobs:
  auto-format:
    name: Auto-format Code
    runs-on: ubuntu-latest
    if: github.event.pull_request.head.repo.full_name == github.repository
    steps:
      - name: Checkout PR branch
        uses: actions/checkout@v4
        with:
          ref: ${{ github.event.pull_request.head.ref }}
          fetch-depth: 0
          token: ${{ secrets.GITHUB_TOKEN }}
      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt
      - name: Apply cargo fmt
        run: |
          set -euo pipefail
          cargo fmt --all
      - name: Detect formatting changes
        id: detect
        run: |
          set -euo pipefail
          if ./scripts/auto-format.sh --check-changes; then
            echo "changed=true" >>"$GITHUB_OUTPUT"
          else
            echo "changed=false" >>"$GITHUB_OUTPUT"
          fi
      - name: Commit and push rustfmt fixes
        if: steps.detect.outputs.changed == 'true'
        run: |
          set -euo pipefail
          git config user.name "github-actions[bot]"
          git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
          msg="$(./scripts/auto-format.sh --commit-message)"
          git commit -am "$msg"
          git push origin "HEAD:${{ github.event.pull_request.head.ref }}"
EOF
}

@test "workflow validator passes on a well-formed fixture" {
  write_good_workflow
  run "$WF_CHECK" --workflow "$TMP_WF/auto-format.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
}

@test "workflow validator fails when pull_request trigger is missing" {
  cat >"$TMP_WF/auto-format.yml" <<'EOF'
name: Auto Format
on:
  push:
    branches: [Develop]
permissions:
  contents: write
jobs:
  auto-format:
    runs-on: ubuntu-latest
    steps:
      - run: cargo fmt --all
EOF
  run "$WF_CHECK" --workflow "$TMP_WF/auto-format.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"pull_request"* ]]
}

@test "workflow validator fails when permissions grant more than needed" {
  cat >"$TMP_WF/auto-format.yml" <<'EOF'
name: Auto Format
on:
  pull_request:
    branches: [Develop]
permissions: write-all
jobs:
  auto-format:
    runs-on: ubuntu-latest
    steps:
      - run: cargo fmt --all
EOF
  run "$WF_CHECK" --workflow "$TMP_WF/auto-format.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"permission"* ]]
}

@test "workflow validator fails when cargo fmt is not invoked" {
  cat >"$TMP_WF/auto-format.yml" <<'EOF'
name: Auto Format
on:
  pull_request:
    branches: [Develop]
permissions:
  contents: write
  pull-requests: read
jobs:
  auto-format:
    runs-on: ubuntu-latest
    steps:
      - run: echo nothing
EOF
  run "$WF_CHECK" --workflow "$TMP_WF/auto-format.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"cargo fmt"* ]]
}

@test "workflow validator fails when the commit step is unconditional" {
  cat >"$TMP_WF/auto-format.yml" <<'EOF'
name: Auto Format
on:
  pull_request:
    branches: [Develop]
permissions:
  contents: write
  pull-requests: read
jobs:
  auto-format:
    runs-on: ubuntu-latest
    steps:
      - run: cargo fmt --all
      - name: Commit
        run: |
          git commit -am "fmt"
          git push
EOF
  run "$WF_CHECK" --workflow "$TMP_WF/auto-format.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"conditional"* || "$output" == *"guard"* ]]
}

@test "workflow validator fails when fork PRs would be pushed to" {
  cat >"$TMP_WF/auto-format.yml" <<'EOF'
name: Auto Format
on:
  pull_request:
    branches: [Develop]
permissions:
  contents: write
  pull-requests: read
jobs:
  auto-format:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Apply cargo fmt
        run: cargo fmt --all
      - name: Detect formatting changes
        id: detect
        run: |
          if ./scripts/auto-format.sh --check-changes; then
            echo "changed=true" >>"$GITHUB_OUTPUT"
          else
            echo "changed=false" >>"$GITHUB_OUTPUT"
          fi
      - name: Commit
        if: steps.detect.outputs.changed == 'true'
        run: git commit -am "fmt" && git push
EOF
  run "$WF_CHECK" --workflow "$TMP_WF/auto-format.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"fork"* || "$output" == *"head.repo"* ]]
}

@test "real repository auto-format workflow validates cleanly" {
  # The shipped .github/workflows/auto-format.yml must satisfy every rule.
  run "$WF_CHECK"
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
  [[ "$output" != *"FAIL"* ]]
}
