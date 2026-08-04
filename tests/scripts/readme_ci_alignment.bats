#!/usr/bin/env bats
# Tests for scripts/check-readme-ci-alignment.sh — Issue #212.
#
# Verifies the validator that keeps the README "step-by-step (matches CI)"
# block aligned with the CI `quality` job. Synthetic README fixtures in a temp
# directory exercise the pass/fail behaviour (exit codes, reported output) so
# the real README is not mutated by the tests.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-readme-ci-alignment.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  REPO_ROOT="${BATS_TEST_DIRNAME}/../.."
  export REPO_ROOT

  TMP_DIR="$(mktemp -d)"
  export TMP_DIR
}

teardown() {
  rm -rf "$TMP_DIR"
}

# Canonical README fixture whose "matches CI" block lists every CI quality
# command. Failure tests mutate this to drop or weaken one command at a time.
write_aligned_readme() {
  local file="$1"
  cat >"$file" <<'EOF'
# Title

Or step-by-step (matches CI):

```bash
export RUSTFLAGS="-D warnings"
cargo deny check
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- \
  -D warnings \
  -D clippy::filter_next \
  -D clippy::collapsible_if
cargo build --workspace
cargo test --workspace --all-features --verbose -- --test-threads=2
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

Trailing prose.
EOF
}

@test "passes on a fully aligned README block" {
  write_aligned_readme "$TMP_DIR/README.md"
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md"
  [ "$status" -eq 0 ]
  [[ "$output" == *"matches the CI quality job"* ]]
}

@test "fails when the rustdoc gate is missing" {
  write_aligned_readme "$TMP_DIR/README.md"
  sed -i.bak '/cargo doc/d' "$TMP_DIR/README.md"
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md"
  [ "$status" -ne 0 ]
  [[ "$output" == *"cargo doc"* ]]
}

# Issue #403: clippy is the strict type-check gate in the CI `quality` job, so a
# standalone `cargo check` step is redundant and was removed from CI. The
# matches-CI block must not reintroduce it, or it drifts back from the job.
@test "fails when a redundant cargo check step is present (Issue #403)" {
  write_aligned_readme "$TMP_DIR/README.md"
  # Re-add the redundant standalone type-check step CI no longer runs.
  sed -i.bak 's|^cargo build --workspace$|cargo check --all-targets --all-features\ncargo build --workspace|' "$TMP_DIR/README.md"
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md"
  [ "$status" -ne 0 ]
  [[ "$output" == *"cargo check"* ]]
  [[ "$output" == *"redundant"* ]]
}

@test "fails when the workspace debug build is missing" {
  write_aligned_readme "$TMP_DIR/README.md"
  sed -i.bak '/cargo build --workspace/d' "$TMP_DIR/README.md"
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md"
  [ "$status" -ne 0 ]
  [[ "$output" == *"cargo build --workspace"* ]]
}

@test "fails when clippy uses the weaker --workspace form instead of CI's" {
  write_aligned_readme "$TMP_DIR/README.md"
  # Replace the CI clippy invocation with the weaker --workspace-only variant.
  python3 - "$TMP_DIR/README.md" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace(
    "cargo clippy --all-targets --all-features -- \\\n"
    "  -D warnings \\\n"
    "  -D clippy::filter_next \\\n"
    "  -D clippy::collapsible_if",
    "cargo clippy --workspace -- -D warnings",
)
with open(path, "w") as fh:
    fh.write(text)
PY
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md"
  [ "$status" -ne 0 ]
  [[ "$output" == *"--workspace"* ]]
}

@test "fails when the matches-CI block is absent" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Title

No matching block here.
EOF
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md"
  [ "$status" -ne 0 ]
  [[ "$output" == *"could not find"* ]]
}

@test "reports an error when the README file does not exist" {
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/does-not-exist.md"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -eq 2 ]
  [[ "$output" == *"Usage"* ]]
}

@test "the real repository README satisfies the alignment check" {
  run "$SCRIPT_UNDER_TEST" --readme "$REPO_ROOT/README.md"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

# --- Issue #506: README ↔ workflow alignment for the CI section.

# Workflow fixture whose shell-checks job runs the runner's pre-installed
# shellcheck binary — i.e. the current ci.yml, with no wrapper action.
write_binary_shellcheck_workflow() {
  mkdir -p "$TMP_DIR/workflows"
  cat >"$TMP_DIR/workflows/ci.yml" <<'EOF'
name: CI
jobs:
  shell-checks:
    runs-on: ubuntu-latest
    steps:
      - name: Run ShellCheck (pre-installed on the runner)
        run: shellcheck --severity=style script.sh
EOF
}

# Minimal stand-in for check-workflow-action-versions.sh's `required:` table.
write_versions_script() {
  cat >"$TMP_DIR/versions.sh" <<'EOF'
#!/usr/bin/env bash
case "$action" in
    actions/dependency-review-action) echo "required:5" ;;
    rustsec/audit-check)              echo "required:2" ;;
esac
EOF
}

@test "fails when the README names a wrapper action no workflow invokes (Issue #506)" {
  write_aligned_readme "$TMP_DIR/README.md"
  write_binary_shellcheck_workflow
  write_versions_script
  printf 'The shell-checks job invokes `ludeeus/action-shellcheck@2.0.0`.\n' \
    >>"$TMP_DIR/README.md"
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md" \
    --workflows "$TMP_DIR/workflows" --versions-script "$TMP_DIR/versions.sh"
  [ "$status" -ne 0 ]
  [[ "$output" == *"ludeeus/action-shellcheck"* ]]
  [[ "$output" == *"no workflow invokes"* ]]
}

@test "allows the wrapper in the README while a workflow still uses it (Issue #506)" {
  write_aligned_readme "$TMP_DIR/README.md"
  write_versions_script
  mkdir -p "$TMP_DIR/workflows"
  cat >"$TMP_DIR/workflows/ci.yml" <<'EOF'
name: CI
jobs:
  shell-checks:
    steps:
      - uses: ludeeus/action-shellcheck@00cae500b08a931fb5698e11e79bfbd38e612a38  # v2.0.0
EOF
  printf 'The shell-checks job invokes `ludeeus/action-shellcheck@2.0.0`.\n' \
    >>"$TMP_DIR/README.md"
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md" \
    --workflows "$TMP_DIR/workflows" --versions-script "$TMP_DIR/versions.sh"
  [ "$status" -eq 0 ]
}

@test "fails when the README understates a required action major (Issue #506)" {
  write_aligned_readme "$TMP_DIR/README.md"
  write_binary_shellcheck_workflow
  write_versions_script
  printf 'Tracked Node 20 exception: `actions/dependency-review-action@v4`.\n' \
    >>"$TMP_DIR/README.md"
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md" \
    --workflows "$TMP_DIR/workflows" --versions-script "$TMP_DIR/versions.sh"
  [ "$status" -ne 0 ]
  [[ "$output" == *"actions/dependency-review-action@v4"* ]]
  [[ "$output" == *"major >= 5"* ]]
}

@test "passes when the README names the required action major (Issue #506)" {
  write_aligned_readme "$TMP_DIR/README.md"
  write_binary_shellcheck_workflow
  write_versions_script
  printf 'Runs `actions/dependency-review-action@v5`; exception: `rustsec/audit-check@v2`.\n' \
    >>"$TMP_DIR/README.md"
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md" \
    --workflows "$TMP_DIR/workflows" --versions-script "$TMP_DIR/versions.sh"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

# A SHA pin such as `@5f6978fa…` starts with a digit; it must not be read as
# "major 5" and compared against the action's floor.
@test "ignores SHA-pinned uses: references when checking majors (Issue #506)" {
  write_aligned_readme "$TMP_DIR/README.md"
  write_binary_shellcheck_workflow
  cat >"$TMP_DIR/versions.sh" <<'EOF'
#!/usr/bin/env bash
case "$action" in
    peter-evans/create-pull-request)  echo "required:8" ;;
esac
EOF
  printf 'uses: peter-evans/create-pull-request@5f6978faf089d4d20b00c7766989d076bb2fc7f1  # v8\n' \
    >>"$TMP_DIR/README.md"
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md" \
    --workflows "$TMP_DIR/workflows" --versions-script "$TMP_DIR/versions.sh"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

@test "reports an error when the workflows directory is missing (Issue #506)" {
  write_aligned_readme "$TMP_DIR/README.md"
  write_versions_script
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md" \
    --workflows "$TMP_DIR/no-such-dir" --versions-script "$TMP_DIR/versions.sh"
  [ "$status" -ne 0 ]
  [[ "$output" == *"workflows directory not found"* ]]
}

@test "reports an error when the action-version validator is missing (Issue #506)" {
  write_aligned_readme "$TMP_DIR/README.md"
  write_binary_shellcheck_workflow
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md" \
    --workflows "$TMP_DIR/workflows" --versions-script "$TMP_DIR/nope.sh"
  [ "$status" -ne 0 ]
  [[ "$output" == *"validator not found"* ]]
}
