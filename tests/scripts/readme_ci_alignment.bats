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
cargo check --all-targets --all-features
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

@test "fails when cargo check is missing" {
  write_aligned_readme "$TMP_DIR/README.md"
  sed -i.bak '/cargo check/d' "$TMP_DIR/README.md"
  run "$SCRIPT_UNDER_TEST" --readme "$TMP_DIR/README.md"
  [ "$status" -ne 0 ]
  [[ "$output" == *"cargo check --all-targets --all-features"* ]]
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
