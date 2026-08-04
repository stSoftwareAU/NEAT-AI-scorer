#!/usr/bin/env bats
# Tests for scripts/check-read-bytes-docs.sh — Issue #504.
#
# Verifies the validator that keeps the README "Large-record hosts" section and
# the docs/performance-baseline.md "Decision (Issue #307)" section aligned with
# the adaptive read-chunk default shipped in rust_scorer/src/read_tuning.rs.
# Synthetic fixtures in a temp directory exercise the pass/fail behaviour, so
# the real documents are never mutated by the tests.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-read-bytes-docs.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  REPO_ROOT="${BATS_TEST_DIRNAME}/../.."
  export REPO_ROOT

  TMP_DIR="$(mktemp -d)"
  export TMP_DIR

  write_source "$TMP_DIR/read_tuning.rs"
  write_readme "$TMP_DIR/README.md"
  write_baseline "$TMP_DIR/performance-baseline.md"
}

teardown() {
  rm -rf "$TMP_DIR"
}

run_check() {
  run "$SCRIPT_UNDER_TEST" \
    --readme "$TMP_DIR/README.md" \
    --baseline "$TMP_DIR/performance-baseline.md" \
    --source "$TMP_DIR/read_tuning.rs"
}

# Minimal stand-in for rust_scorer/src/read_tuning.rs — the constants are the
# source of truth the documents must agree with.
write_source() {
  cat >"$1" <<'EOF'
const DEFAULT_READ_BYTES: usize = 2 * 1024 * 1024;
const LARGE_RECORD_BYTES_THRESHOLD: usize = 8000;
const LARGE_RECORD_DEFAULT_READ_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_READ_BYTES: usize = 64 * 1024 * 1024;
EOF
}

# README fixture that documents the shipped adaptive default correctly.
write_readme() {
  cat >"$1" <<'EOF'
# Title

### Large-record hosts: adaptive `NEAT_SCORER_READ_BYTES` default

Records of 8000 bytes or more default to 32 MiB reads when the env var is
unset (`rust_scorer/src/read_tuning.rs`); smaller records keep the 2 MiB
default. `NEAT_SCORER_READ_BYTES` still overrides, clamped to the 64 MiB cap.

## Next section

Unrelated prose.
EOF
}

# performance-baseline fixture: historical decision text preserved, with the
# dated supersession note appended beneath it.
write_baseline() {
  cat >"$1" <<'EOF'
# Baseline

### Decision (Issue #307)

The clear gain qualifies, but the **global default stays 2 MiB** and no
auto-tuner ships.

> **Superseded (2026-08-04, Issue #504).** The 2-MiB-only decision above was
> later superseded by the adaptive default in `read_tuning.rs`.

## Next section

Unrelated prose.
EOF
}

@test "passes on documents aligned with the shipped constants" {
  run_check
  [ "$status" -eq 0 ]
  [[ "$output" == *"agree with"* ]]
}

@test "fails when the README omits the large-record threshold" {
  sed -i.bak 's/8000 bytes or more/large records/' "$TMP_DIR/README.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"8000"* ]]
}

@test "fails when the README omits the adaptive 32 MiB default" {
  sed -i.bak 's/32 MiB reads/2 MiB reads/' "$TMP_DIR/README.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"32 MiB"* ]]
}

@test "fails when the README omits the MAX_READ_BYTES cap" {
  sed -i.bak 's/the 64 MiB cap/the documented cap/' "$TMP_DIR/README.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"64 MiB"* ]]
}

@test "fails when the README does not point at read_tuning as the constants' home" {
  sed -i.bak 's|(`rust_scorer/src/read_tuning.rs`)||' "$TMP_DIR/README.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"read_tuning"* ]]
}

@test "fails when the README revives the superseded 'left at 2 MiB' claim" {
  sed -i.bak \
    's/smaller records keep the 2 MiB/the global default is intentionally left at 2 MiB, so smaller records keep the 2 MiB/' \
    "$TMP_DIR/README.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"left at 2 MiB"* ]]
}

@test "fails when the README section is absent entirely" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Title

No read-tuning section here.
EOF
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"could not find"* ]]
}

@test "fails when the baseline decision carries no supersession note" {
  sed -i.bak '/Superseded/,+1d' "$TMP_DIR/performance-baseline.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"superseded"* ]]
}

@test "fails when the historical 2 MiB decision text was overwritten" {
  sed -i.bak 's/\*\*global default stays 2 MiB\*\*/global default is adaptive/' \
    "$TMP_DIR/performance-baseline.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"historical"* ]]
}

@test "reports an error when a document is missing" {
  rm -f "$TMP_DIR/README.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -eq 2 ]
  [[ "$output" == *"Usage"* ]]
}

@test "the real repository documents satisfy the check" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
