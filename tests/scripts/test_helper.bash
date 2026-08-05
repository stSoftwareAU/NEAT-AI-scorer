#!/usr/bin/env bash
# Shared BATS assertions for the guard-script CLI harness — Issue #512.
#
# Every `scripts/check-*.sh` validator sources `scripts/lib/check-harness.sh`
# and therefore shares one CLI contract: a missing target exits non-zero with a
# "not found" message, and an unknown flag exits non-zero after printing the
# usage block. Those two contract tests were re-stated verbatim in 30-odd .bats
# files; they now live here, so a change to the contract is a single edit.
#
# `run` exports `$status` and `$output` to the calling test, so a suite with a
# stricter expectation can call the assertion and then add its own checks.
#
# Load from a .bats file with:  load 'test_helper'

# assert_missing_target_rejected <script> [args...] — the shared not-found guard.
assert_missing_target_rejected() {
  run "$@"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

# assert_unknown_flag_rejected <script> — the shared unknown-flag contract.
assert_unknown_flag_rejected() {
  run "$1" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}
