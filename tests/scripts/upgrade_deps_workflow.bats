#!/usr/bin/env bats
# Tests for scripts/check-upgrade-deps-workflow.sh — Issue #90.
#
# The weekly `upgrade-dependencies.yml` workflow that produced PR #89
# committed three classes of junk alongside the legitimate Cargo.lock
# bump: workflow log artefacts (`upgrade-dry-run.txt`, `upgrade.log`)
# and the sibling NEAT-AI-core checkout (as a stray gitlink). This
# validator enforces three rules so the same workflow cannot produce
# the same misleading PR again:
#
#   1. The workflow writes `cargo upgrade` logs OUTSIDE the repo (e.g.
#      to `${{ runner.temp }}/`), not into the worktree root.
#   2. The PR-creation step restricts the staged paths to Cargo files
#      via `add-paths:` — so the NEAT-AI-core sibling and any other
#      runner debris cannot leak into the commit.
#   3. The PR body actually lists what changed (a `git diff` over the
#      Cargo files), rather than just the dry-run summary that misled
#      the reader of PR #89.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-upgrade-deps-workflow.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST" 2>/dev/null || true

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

write_good_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Upgrade Cargo Dependencies
on:
  schedule:
    - cron: "0 6 * * 1"
  workflow_dispatch:
permissions:
  contents: write
  pull-requests: write
jobs:
  upgrade:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: actions/checkout@v5
        with:
          repository: stSoftwareAU/NEAT-AI-core
          path: NEAT-AI-core
      - name: Upgrade dependencies
        run: |
          set -euo pipefail
          cargo upgrade --dry-run 2>&1 | tee "${RUNNER_TEMP}/upgrade-dry-run.txt"
          cargo upgrade 2>&1 | tee "${RUNNER_TEMP}/upgrade.log"
      - name: Check for changes
        id: changes
        run: |
          if git diff --quiet -- 'Cargo.toml' '**/Cargo.toml' 'Cargo.lock'; then
            echo "changed=false" >> "$GITHUB_OUTPUT"
          else
            echo "changed=true" >> "$GITHUB_OUTPUT"
          fi
      - name: Build summary
        if: steps.changes.outputs.changed == 'true'
        id: summary
        run: |
          {
            echo 'body<<BODY_EOF'
            echo '## Dependency Upgrades'
            echo '### Files changed'
            echo '```'
            git diff --stat -- 'Cargo.toml' '**/Cargo.toml' 'Cargo.lock'
            echo '```'
            echo 'BODY_EOF'
          } >> "$GITHUB_OUTPUT"
      - name: Create pull request
        if: steps.changes.outputs.changed == 'true'
        uses: peter-evans/create-pull-request@v8
        with:
          add-paths: |
            Cargo.toml
            **/Cargo.toml
            Cargo.lock
          branch: chore/upgrade-dependencies
          base: Develop
          title: "chore: upgrade Cargo dependencies"
          body: ${{ steps.summary.outputs.body }}
EOF
}

@test "passes when the workflow writes logs to RUNNER_TEMP, restricts add-paths, and diffs Cargo files in the body" {
  write_good_workflow "$TMP_WF/upgrade-dependencies.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/upgrade-dependencies.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
}

@test "fails when cargo upgrade logs are written into the repo root (tee upgrade-dry-run.txt)" {
  cat >"$TMP_WF/bad.yml" <<'EOF'
name: Upgrade
on: [workflow_dispatch]
jobs:
  upgrade:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - name: Upgrade dependencies
        run: |
          cargo upgrade --dry-run 2>&1 | tee upgrade-dry-run.txt
          cargo upgrade 2>&1 | tee upgrade.log
      - name: Create pull request
        uses: peter-evans/create-pull-request@v8
        with:
          add-paths: |
            Cargo.toml
            Cargo.lock
          branch: chore/upgrade-dependencies
          body: |
            ```
            $(git diff --stat -- 'Cargo.lock')
            ```
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/bad.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"upgrade"* ]]
  [[ "$output" == *"repo root"* || "$output" == *"RUNNER_TEMP"* ]]
}

@test "fails when create-pull-request omits add-paths (would stage all changed files including NEAT-AI-core)" {
  cat >"$TMP_WF/bad.yml" <<'EOF'
name: Upgrade
on: [workflow_dispatch]
jobs:
  upgrade:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - name: Upgrade dependencies
        run: |
          cargo upgrade --dry-run 2>&1 | tee "${RUNNER_TEMP}/upgrade-dry-run.txt"
          cargo upgrade 2>&1 | tee "${RUNNER_TEMP}/upgrade.log"
      - name: Create pull request
        uses: peter-evans/create-pull-request@v8
        with:
          branch: chore/upgrade-dependencies
          body: |
            ```
            $(git diff --stat -- 'Cargo.lock')
            ```
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/bad.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"add-paths"* ]]
}

@test "fails when add-paths includes a path other than Cargo.toml/Cargo.lock" {
  cat >"$TMP_WF/bad.yml" <<'EOF'
name: Upgrade
on: [workflow_dispatch]
jobs:
  upgrade:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - name: Upgrade dependencies
        run: |
          cargo upgrade --dry-run 2>&1 | tee "${RUNNER_TEMP}/upgrade-dry-run.txt"
          cargo upgrade 2>&1 | tee "${RUNNER_TEMP}/upgrade.log"
      - name: Create pull request
        uses: peter-evans/create-pull-request@v8
        with:
          add-paths: |
            Cargo.toml
            Cargo.lock
            NEAT-AI-core
            upgrade.log
          branch: chore/upgrade-dependencies
          body: |
            ```
            $(git diff --stat -- 'Cargo.lock')
            ```
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/bad.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"add-paths"* ]]
  [[ "$output" == *"NEAT-AI-core"* || "$output" == *"unexpected"* ]]
}

@test "fails when the PR body does not include a Cargo diff (only the dry-run summary)" {
  cat >"$TMP_WF/bad.yml" <<'EOF'
name: Upgrade
on: [workflow_dispatch]
jobs:
  upgrade:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - name: Upgrade dependencies
        run: |
          cargo upgrade --dry-run 2>&1 | tee "${RUNNER_TEMP}/upgrade-dry-run.txt"
          cargo upgrade 2>&1 | tee "${RUNNER_TEMP}/upgrade.log"
      - name: Build summary
        id: summary
        run: |
          {
            echo 'body<<BODY_EOF'
            echo '## Dependency Upgrades'
            echo 'See the dry-run log.'
            echo 'BODY_EOF'
          } >> "$GITHUB_OUTPUT"
      - name: Create pull request
        uses: peter-evans/create-pull-request@v8
        with:
          add-paths: |
            Cargo.toml
            Cargo.lock
          branch: chore/upgrade-dependencies
          body: ${{ steps.summary.outputs.body }}
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/bad.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"diff"* ]]
}

@test "reports an error when the workflow file does not exist" {
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/missing.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* || "$output" == *"Unknown"* ]]
}

@test "real shipped upgrade-dependencies workflow satisfies every rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK"* ]]
  [[ "$output" != *"FAIL"* ]]
}
