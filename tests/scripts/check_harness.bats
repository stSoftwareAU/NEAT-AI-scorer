#!/usr/bin/env bats
# Tests for scripts/lib/check-harness.sh — Issue #512.
#
# The harness is sourced, not executed, so every test drives it through a
# throwaway validator script written into a temp directory. That exercises the
# real contract end-to-end — argument parsing, the not-found guard, and the
# OK/FAIL accumulate-and-report protocol — via exit codes and output rather
# than by inspecting the library source.

bats_require_minimum_version 1.5.0

setup() {
  HARNESS="${BATS_TEST_DIRNAME}/../../scripts/lib/check-harness.sh"
  TMP_DIR="$(mktemp -d)"
  export HARNESS TMP_DIR
}

teardown() {
  rm -rf "$TMP_DIR"
}

# Write a minimal validator that uses the whole harness: usage(), the
# single-target argument loop, the not-found guard, and one ok/fail rule.
write_validator() {
  local script="$TMP_DIR/validator.sh"
  cat >"$script" <<EOF
#!/usr/bin/env bash
set -euo pipefail
# shellcheck source=scripts/lib/check-harness.sh
source "$HARNESS"

usage() {
  cat <<'USAGE'
Usage: validator.sh [--workflow PATH]
USAGE
}

parse_check_args --workflow "README.md" "\$@"
check_require_file "\$CHECK_TARGET"
CHECK_SUBJECT="\$CHECK_TARGET"

echo "TARGET=\$CHECK_TARGET"
if grep -q 'good' "\$CHECK_TARGET"; then
  ok "contains the marker"
else
  fail "missing the marker"
fi
exit "\$EXIT_CODE"
EOF
  chmod +x "$script"
  echo "$script"
}

@test "resolves the default target relative to the repo root" {
  script="$(write_validator)"
  run "$script"
  [ "$status" -eq 0 ] || [ "$status" -eq 1 ]
  [[ "$output" == *"TARGET=/"* ]]
  [[ "$output" == *"/README.md"* ]]
}

@test "an explicit flag overrides the default target" {
  script="$(write_validator)"
  echo "good" >"$TMP_DIR/target.txt"
  run "$script" --workflow "$TMP_DIR/target.txt"
  [ "$status" -eq 0 ]
  [[ "$output" == *"TARGET=$TMP_DIR/target.txt"* ]]
}

@test "--help prints usage and exits zero" {
  script="$(write_validator)"
  run "$script" --help
  [ "$status" -eq 0 ]
  [[ "$output" == *"Usage: validator.sh"* ]]
}

@test "an unknown flag names the argument, prints usage and exits 2" {
  script="$(write_validator)"
  run "$script" --nonsense
  [ "$status" -eq 2 ]
  [[ "$output" == *"Unknown argument: --nonsense"* ]]
  [[ "$output" == *"Usage:"* ]]
}

@test "a flag with no value fails with a message instead of an unbound variable" {
  script="$(write_validator)"
  run "$script" --workflow
  [ "$status" -eq 2 ]
  [[ "$output" == *"--workflow requires a PATH argument"* ]]
}

@test "a missing target file exits 2 with a not-found message" {
  script="$(write_validator)"
  run "$script" --workflow "$TMP_DIR/does-not-exist.yml"
  [ "$status" -eq 2 ]
  [[ "$output" == *"not found: $TMP_DIR/does-not-exist.yml"* ]]
}

@test "a satisfied rule reports OK on stdout and exits zero" {
  script="$(write_validator)"
  echo "good" >"$TMP_DIR/target.txt"
  run "$script" --workflow "$TMP_DIR/target.txt"
  [ "$status" -eq 0 ]
  [[ "$output" == *"OK   $TMP_DIR/target.txt: contains the marker"* ]]
}

@test "a violated rule reports FAIL on stderr and exits 1" {
  script="$(write_validator)"
  echo "bad" >"$TMP_DIR/target.txt"
  run --separate-stderr "$script" --workflow "$TMP_DIR/target.txt"
  [ "$status" -eq 1 ]
  [[ "$stderr" == *"FAIL $TMP_DIR/target.txt: missing the marker"* ]]
  [[ "$output" != *"FAIL"* ]]
}

@test "failures accumulate rather than aborting on the first one" {
  script="$TMP_DIR/multi.sh"
  cat >"$script" <<EOF
#!/usr/bin/env bash
set -euo pipefail
source "$HARNESS"
usage() { echo "Usage: multi.sh"; }
CHECK_SUBJECT="subject"
fail "first violation"
fail "second violation"
ok "third rule holds"
exit "\$EXIT_CODE"
EOF
  chmod +x "$script"
  run "$script"
  [ "$status" -eq 1 ]
  [[ "$output" == *"FAIL subject: first violation"* ]]
  [[ "$output" == *"FAIL subject: second violation"* ]]
  [[ "$output" == *"OK   subject: third rule holds"* ]]
}

@test "an empty subject drops the prefix from OK and FAIL lines" {
  script="$TMP_DIR/nosubject.sh"
  cat >"$script" <<EOF
#!/usr/bin/env bash
set -euo pipefail
source "$HARNESS"
usage() { echo "Usage: nosubject.sh"; }
ok "rule holds"
fail "rule broken"
exit "\$EXIT_CODE"
EOF
  chmod +x "$script"
  run "$script"
  [ "$status" -eq 1 ]
  [[ "$output" == *"OK   rule holds"* ]]
  [[ "$output" == *"FAIL rule broken"* ]]
}

@test "check_require_dir guards a missing directory with a labelled message" {
  script="$TMP_DIR/dir.sh"
  cat >"$script" <<EOF
#!/usr/bin/env bash
set -euo pipefail
source "$HARNESS"
usage() { echo "Usage: dir.sh"; }
check_require_dir "$TMP_DIR/absent" "Workflows directory"
exit 0
EOF
  chmod +x "$script"
  run "$script"
  [ "$status" -eq 2 ]
  [[ "$output" == *"Workflows directory not found: $TMP_DIR/absent"* ]]
}

@test "an empty default leaves the target unset for caller-side resolution" {
  script="$TMP_DIR/nodefault.sh"
  cat >"$script" <<EOF
#!/usr/bin/env bash
set -euo pipefail
source "$HARNESS"
usage() { echo "Usage: nodefault.sh"; }
parse_check_args --codeowners "" "\$@"
echo "TARGET=[\$CHECK_TARGET]"
exit 0
EOF
  chmod +x "$script"
  run "$script"
  [ "$status" -eq 0 ]
  [[ "$output" == *"TARGET=[]"* ]]
}
