#!/usr/bin/env bats
# Tests for scripts/run-benches.sh — Issue #36.
#
# The script wraps `cargo bench -p rust_scorer` so the bench fixture configuration
# stays in one place. We don't actually invoke `cargo bench` here (Criterion runs
# are far too slow for a unit test) — instead we shim `cargo` on PATH and assert:
#   * the script forwards the right arguments to `cargo bench`,
#   * the BENCH_SCORING_* env vars are passed through,
#   * the script exits non-zero when `cargo bench` fails.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/run-benches.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_BIN="$(mktemp -d)"
  export TMP_BIN
  CARGO_LOG="${TMP_BIN}/cargo-log"
  export CARGO_LOG

  cat >"${TMP_BIN}/cargo" <<EOF
#!/bin/bash
{
  echo "ARGS: \$*"
  echo "BENCH_SCORING_BYTES=\${BENCH_SCORING_BYTES:-unset}"
  echo "BENCH_SCORING_INPUTS=\${BENCH_SCORING_INPUTS:-unset}"
} >>"${CARGO_LOG}"
exit "\${SHIM_CARGO_EXIT:-0}"
EOF
  chmod +x "${TMP_BIN}/cargo"
  PATH="${TMP_BIN}:$PATH"
  export PATH
  # Disable .cargo/env sourcing inside the script (would prepend the real cargo).
  HOME_BACKUP="$HOME"
  HOME="$TMP_BIN"
  export HOME HOME_BACKUP
}

teardown() {
  rm -rf "$TMP_BIN"
  HOME="${HOME_BACKUP:-$HOME}"
}

@test "invokes cargo bench -p rust_scorer with no extra args" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  grep -q "ARGS: bench -p rust_scorer" "$CARGO_LOG"
}

@test "forwards extra arguments to cargo bench" {
  run "$SCRIPT_UNDER_TEST" -- score_from_json_fused
  [ "$status" -eq 0 ]
  grep -q "ARGS: bench -p rust_scorer -- score_from_json_fused" "$CARGO_LOG"
}

@test "passes BENCH_SCORING_BYTES through to cargo bench" {
  BENCH_SCORING_BYTES=200000000 run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  grep -q "BENCH_SCORING_BYTES=200000000" "$CARGO_LOG"
}

@test "propagates non-zero exit status from cargo bench" {
  SHIM_CARGO_EXIT=2 run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 2 ]
}

@test "prints the active bench fixture configuration" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" == *"Running rust_scorer Criterion benches"* ]]
  [[ "$output" == *"BENCH_SCORING_BYTES="* ]]
  [[ "$output" == *"BENCH_SCORING_INPUTS="* ]]
  [[ "$output" == *"BENCH_SCORING_OUTPUTS="* ]]
  [[ "$output" == *"BENCH_SCORING_HIDDEN="* ]]
}
