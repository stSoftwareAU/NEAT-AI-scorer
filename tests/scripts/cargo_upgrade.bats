#!/usr/bin/env bats
# Tests for scripts/cargo-upgrade.sh — the opt-in dependency upgrade step
# extracted from quality.sh (Issue #210).
#
# The default local gate must be read-only against Cargo.lock / Cargo.toml:
# running ./quality.sh to validate an unrelated change must never mutate the
# lockfile or manifests. These tests stub `cargo` and `cargo-upgrade` on PATH
# and assert which subcommands the script actually invokes for each opt-in
# state, so behaviour (exit codes, output, mutating calls) is verified
# end-to-end rather than by inspecting source text.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/cargo-upgrade.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  STUB_DIR="$(mktemp -d)"
  export STUB_DIR
  export CARGO_LOG="$STUB_DIR/cargo.log"
  : >"$CARGO_LOG"

  # `cargo` stub records every invocation so we can assert on the
  # `upgrade` / `update` subcommands the script would have run.
  cat >"$STUB_DIR/cargo" <<EOF
#!/bin/bash
echo "cargo \$*" >>"$CARGO_LOG"
EOF
  chmod +x "$STUB_DIR/cargo"
}

teardown() {
  rm -rf "$STUB_DIR"
}

# Install a `cargo-upgrade` stub so `command -v cargo-upgrade` succeeds.
install_cargo_upgrade_stub() {
  cat >"$STUB_DIR/cargo-upgrade" <<EOF
#!/bin/bash
echo "cargo-upgrade \$*" >>"$CARGO_LOG"
EOF
  chmod +x "$STUB_DIR/cargo-upgrade"
}

@test "default run is read-only: no cargo upgrade/update is invoked" {
  install_cargo_upgrade_stub
  PATH="$STUB_DIR:$PATH" run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" == *"Skipping dependency upgrade"* ]]
  # Nothing should have touched cargo at all.
  [ ! -s "$CARGO_LOG" ]
}

@test "--upgrade opts in and runs cargo upgrade + cargo update" {
  install_cargo_upgrade_stub
  PATH="$STUB_DIR:$PATH" run "$SCRIPT_UNDER_TEST" --upgrade
  [ "$status" -eq 0 ]
  grep -q "cargo upgrade --incompatible" "$CARGO_LOG"
  grep -q "cargo update" "$CARGO_LOG"
}

@test "QUALITY_UPGRADE=1 opts in and runs cargo upgrade + cargo update" {
  install_cargo_upgrade_stub
  QUALITY_UPGRADE=1 PATH="$STUB_DIR:$PATH" run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  grep -q "cargo upgrade --incompatible" "$CARGO_LOG"
  grep -q "cargo update" "$CARGO_LOG"
}

@test "opt-in without cargo-edit warns and does not mutate" {
  # No cargo-upgrade stub installed; restrict PATH so the real one (if any)
  # is not found either.
  PATH="$STUB_DIR:/usr/bin:/bin" run "$SCRIPT_UNDER_TEST" --upgrade
  [ "$status" -eq 0 ]
  [[ "$output" == *"cargo-edit not installed"* ]]
  [ ! -s "$CARGO_LOG" ]
}

@test "unrelated arguments do not trigger an upgrade" {
  install_cargo_upgrade_stub
  PATH="$STUB_DIR:$PATH" run "$SCRIPT_UNDER_TEST" --some-other-flag
  [ "$status" -eq 0 ]
  [[ "$output" == *"Skipping dependency upgrade"* ]]
  [ ! -s "$CARGO_LOG" ]
}
