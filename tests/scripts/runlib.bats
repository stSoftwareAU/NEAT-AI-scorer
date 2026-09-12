#!/usr/bin/env bats
# Hermetic coverage for scripts/runlib.sh (Issue #629).
#
# Drives scripts/test-runlib.sh so CI's bats job and ./quality.sh share one
# suite. The helper never runs a real cargo build.

bats_require_minimum_version 1.5.0

@test "runlib already-installed path and missing stamp are hermetic" {
  run "${BATS_TEST_DIRNAME}/../../scripts/test-runlib.sh"
  echo "$output"
  [ "$status" -eq 0 ]
}
