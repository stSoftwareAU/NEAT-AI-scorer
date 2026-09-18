#!/usr/bin/env bats
# Tests for bump-deps.sh — Cargo dependency refresh helper (Issue #55).
# Exercises the script against temporary fixtures so behaviour (exit codes,
# output, manifest mutations) is verified end-to-end.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../bump-deps.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_REPO="$(mktemp -d)"
  export TMP_REPO
  mkdir -p "$TMP_REPO/rust_scorer"
}

teardown() {
  rm -rf "$TMP_REPO"
}

write_path_manifest() {
  cat >"$TMP_REPO/rust_scorer/Cargo.toml" <<'EOF'
[package]
name = "rust_scorer"
version = "0.0.1"
edition = "2024"

[dependencies]
neat-core = { path = "../../NEAT-AI-core/neat-core" }
clap = "4"
EOF
}

write_pinned_manifest() {
  local sha="$1"
  cat >"$TMP_REPO/rust_scorer/Cargo.toml" <<EOF
[package]
name = "rust_scorer"
version = "0.0.1"
edition = "2024"

[dependencies]
neat-core = { git = "https://github.com/stSoftwareAU/NEAT-AI-core", rev = "${sha}" }
clap = "4"
EOF
}

write_pinned_section_manifest() {
  local sha="$1"
  cat >"$TMP_REPO/rust_scorer/Cargo.toml" <<EOF
[package]
name = "rust_scorer"
version = "0.0.1"
edition = "2024"

[dependencies]
clap = "4"

[dependencies.neat-core]
git = "https://github.com/stSoftwareAU/NEAT-AI-core"
rev = "${sha}"
EOF
}

@test "shows usage with --help" {
  run "$SCRIPT_UNDER_TEST" --help
  [ "$status" -eq 0 ]
  [[ "$output" == *"Usage:"* ]]
  [[ "$output" == *"--quarantine-hours"* ]]
}

@test "rejects unknown options" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"unknown option"* ]]
}

@test "rejects non-integer quarantine hours" {
  write_path_manifest
  run "$SCRIPT_UNDER_TEST" --quarantine-hours abc \
    --skip-external --skip-audit --skip-build \
    --repo "$TMP_REPO"
  [ "$status" -ne 0 ]
  [[ "$output" == *"quarantine-hours"* ]]
}

@test "internal bump: path-only neat-core dep is reported as no-op" {
  write_path_manifest
  run "$SCRIPT_UNDER_TEST" \
    --skip-external --skip-audit --skip-build \
    --repo "$TMP_REPO" \
    --neat-core-sha "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
  [ "$status" -eq 0 ]
  [[ "$output" == *"path dependency"* ]]
  [[ "$output" == *"no bumps"* ]]
}

@test "internal bump: rev pin advances when upstream Develop has moved" {
  write_pinned_manifest "1111111111111111111111111111111111111111"
  run "$SCRIPT_UNDER_TEST" \
    --skip-external --skip-audit --skip-build \
    --repo "$TMP_REPO" \
    --neat-core-sha "2222222222222222222222222222222222222222"
  [ "$status" -eq 0 ]
  [[ "$output" == *"1111111"* ]]
  [[ "$output" == *"2222222"* ]]
  grep -q '2222222222222222222222222222222222222222' "$TMP_REPO/rust_scorer/Cargo.toml"
  ! grep -q '1111111111111111111111111111111111111111' "$TMP_REPO/rust_scorer/Cargo.toml"
}

@test "internal bump: rev pin no-op when SHA already matches Develop HEAD" {
  write_pinned_manifest "abcdef0123456789abcdef0123456789abcdef01"
  run "$SCRIPT_UNDER_TEST" \
    --skip-external --skip-audit --skip-build \
    --repo "$TMP_REPO" \
    --neat-core-sha "abcdef0123456789abcdef0123456789abcdef01"
  [ "$status" -eq 0 ]
  [[ "$output" == *"already pinned"* ]]
  [[ "$output" == *"no bumps"* ]]
}

@test "internal bump: handles [dependencies.neat-core] section form" {
  write_pinned_section_manifest "1111111111111111111111111111111111111111"
  run "$SCRIPT_UNDER_TEST" \
    --skip-external --skip-audit --skip-build \
    --repo "$TMP_REPO" \
    --neat-core-sha "2222222222222222222222222222222222222222"
  [ "$status" -eq 0 ]
  [[ "$output" == *"1111111"* ]]
  [[ "$output" == *"2222222"* ]]
  grep -q '2222222222222222222222222222222222222222' "$TMP_REPO/rust_scorer/Cargo.toml"
}

@test "all skip flags: produces a clean no-op" {
  write_path_manifest
  run "$SCRIPT_UNDER_TEST" \
    --skip-internal --skip-external --skip-audit --skip-build \
    --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
  [[ "$output" == *"no bumps"* ]]
}

@test "check-published: ancient timestamp is older than quarantine (exit 0)" {
  run "$SCRIPT_UNDER_TEST" --check-published "2020-01-01T00:00:00Z" 24
  [ "$status" -eq 0 ]
}

@test "check-published: very recent timestamp is within quarantine (exit 1)" {
  # A timestamp ~1 second ago is well inside any positive quarantine window.
  recent="$(python3 -c 'import datetime; print(datetime.datetime.now(datetime.timezone.utc).isoformat())')"
  run "$SCRIPT_UNDER_TEST" --check-published "$recent" 24
  [ "$status" -eq 1 ]
}

@test "check-published: quarantine of zero hours always allows the bump" {
  recent="$(python3 -c 'import datetime; print(datetime.datetime.now(datetime.timezone.utc).isoformat())')"
  run "$SCRIPT_UNDER_TEST" --check-published "$recent" 0
  [ "$status" -eq 0 ]
}

@test "check-published: invalid timestamp surfaces an error" {
  run "$SCRIPT_UNDER_TEST" --check-published "not-a-date" 24
  [ "$status" -ne 0 ]
}

# Issue #101: the weekly Upgrade Cargo Dependencies workflow must route its
# `cargo upgrade` invocation through bump-deps.sh so the same quarantine
# window applies to scheduled bumps as worker-initiated bumps. The new
# `--cargo-upgrade` flag selects manifest-level upgrades while keeping the
# per-crate quarantine gate.

@test "accepts --cargo-upgrade flag (Issue #101)" {
  write_path_manifest
  run "$SCRIPT_UNDER_TEST" \
    --cargo-upgrade \
    --skip-internal --skip-external --skip-audit --skip-build \
    --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
}

@test "--help advertises --cargo-upgrade (Issue #101)" {
  run "$SCRIPT_UNDER_TEST" --help
  [ "$status" -eq 0 ]
  [[ "$output" == *"--cargo-upgrade"* ]]
}

# --- Issue #619 -------------------------------------------------------------
# `bump-deps.sh` exited non-zero on every run, so the worker reverted the bump
# each time and dependency bumps were effectively disabled for this repo. Two
# root causes are covered below:
#
#   1. `cargo audit` is not installed on the unattended PATH. A missing tool is
#      a tooling gap, not a bump rejection — the advisory scan is deferred to
#      the CI job that owns it (`.github/workflows/cargo-audit.yml`).
#   2. Interlocked crate families (wasm-bindgen / js-sys / web-sys pin each
#      other with `=` requirements) can never be bumped one crate at a time, so
#      every per-crate `--precise` apply is rejected and the real cargo error
#      was discarded.

# A scripted stand-in for cargo so the stages can be driven without a network
# or a real workspace. Behaviour is selected by FAKE_CARGO_* environment
# variables exported by each test.
write_fake_cargo() {
  mkdir -p "$TMP_REPO/bin"
  cat >"$TMP_REPO/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -u
sub="${1:-}"
shift || true
case "$sub" in
  audit)
    if [[ -n "${FAKE_CARGO_AUDIT_MISSING:-}" ]]; then
      echo "error: no such command: \`audit\`" >&2
      exit 101
    fi
    if [[ "${1:-}" == "--version" ]]; then
      echo "cargo-audit-audit 0.21.0"
      exit 0
    fi
    if [[ -n "${FAKE_CARGO_AUDIT_ADVISORY:-}" ]]; then
      printf 'Crate:     badcrate\n  ID:        RUSTSEC-2024-0001\n'
      printf '  Crate:     badcrate\n'
      exit 1
    fi
    exit 0
    ;;
  update)
    args=("$@")
    if [[ " ${args[*]} " == *" --dry-run "* ]]; then
      if [[ "${FAKE_DRY_RC:-0}" != "0" ]]; then
        cat "${FAKE_DRY_LOG:-/dev/null}" >&2
        exit "${FAKE_DRY_RC}"
      fi
      cat "${FAKE_DRY_LOG:-/dev/null}"
      exit 0
    fi
    if [[ " ${args[*]} " == *" --precise "* ]]; then
      if [[ -n "${FAKE_PRECISE_FAIL:-}" ]]; then
        echo "error: failed to select a version for the requirement \`locked = \"=1.0.0\"\`" >&2
        exit 101
      fi
      exit 0
    fi
    if [[ -n "${FAKE_GROUP_SCRIPT:-}" ]]; then
      bash "$FAKE_GROUP_SCRIPT" "$@"
      exit $?
    fi
    echo "error: grouped update not scripted" >&2
    exit 101
    ;;
  upgrade)
    [[ -n "${FAKE_CARGO_UPGRADE_MISSING:-}" ]] && exit 101
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
EOF
  chmod +x "$TMP_REPO/bin/cargo"
  PATH="$TMP_REPO/bin:$PATH"
  export PATH
}

# A Cargo.lock holding an interlocked wasm-bindgen family at the older
# versions, plus an unrelated crate the regrouped resolve must not disturb.
write_interlocked_lock() {
  cat >"$TMP_REPO/Cargo.lock" <<'EOF'
version = 4

[[package]]
name = "js-sys"
version = "0.3.104"

[[package]]
name = "syn"
version = "2.0.119"

[[package]]
name = "wasm-bindgen"
version = "0.2.127"
EOF
}

write_interlocked_dry_log() {
  cat >"$TMP_REPO/dry.log" <<'EOF'
    Updating crates.io index
    Updating js-sys v0.3.104 -> v0.3.105
    Updating wasm-bindgen v0.2.127 -> v0.2.128
EOF
  FAKE_DRY_LOG="$TMP_REPO/dry.log"
  export FAKE_DRY_LOG
}

# Empty fixture directory: every publish-time lookup misses and is treated as
# ancient, so the quarantine gate clears both candidates.
use_ancient_publish_fixture() {
  mkdir -p "$TMP_REPO/publish"
  BUMP_DEPS_PUBLISH_FIXTURE="$TMP_REPO/publish"
  export BUMP_DEPS_PUBLISH_FIXTURE
}

@test "audit stage: a missing cargo-audit is a skip, not a bump rejection (Issue #619)" {
  write_path_manifest
  write_fake_cargo
  FAKE_CARGO_AUDIT_MISSING=1
  export FAKE_CARGO_AUDIT_MISSING
  run "$SCRIPT_UNDER_TEST" \
    --skip-internal --skip-external --skip-build \
    --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
  [[ "$output" == *"audit: SKIPPED"* ]]
  [[ "$output" == *"cargo-audit"* ]]
  [[ "$output" == *"no bumps"* ]]
}

@test "audit stage: --require-audit makes a missing cargo-audit fatal (Issue #619)" {
  write_path_manifest
  write_fake_cargo
  FAKE_CARGO_AUDIT_MISSING=1
  export FAKE_CARGO_AUDIT_MISSING
  run "$SCRIPT_UNDER_TEST" \
    --require-audit \
    --skip-internal --skip-external --skip-build \
    --repo "$TMP_REPO"
  [ "$status" -eq 1 ]
  [[ "$output" == *"cargo audit not available"* ]]
}

@test "audit stage: BUMP_DEPS_REQUIRE_AUDIT=1 makes a missing cargo-audit fatal (Issue #619)" {
  write_path_manifest
  write_fake_cargo
  FAKE_CARGO_AUDIT_MISSING=1
  BUMP_DEPS_REQUIRE_AUDIT=1
  export FAKE_CARGO_AUDIT_MISSING BUMP_DEPS_REQUIRE_AUDIT
  run "$SCRIPT_UNDER_TEST" \
    --skip-internal --skip-external --skip-build \
    --repo "$TMP_REPO"
  [ "$status" -eq 1 ]
  [[ "$output" == *"cargo audit not available"* ]]
}

@test "audit stage: an installed cargo-audit still runs and reports ok (Issue #619)" {
  write_path_manifest
  write_fake_cargo
  run "$SCRIPT_UNDER_TEST" \
    --skip-internal --skip-external --skip-build \
    --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
  [[ "$output" == *"audit: ok"* ]]
}

@test "audit stage: a reported advisory still fails the bump (Issue #619)" {
  write_path_manifest
  write_fake_cargo
  FAKE_CARGO_AUDIT_ADVISORY=1
  export FAKE_CARGO_AUDIT_ADVISORY
  run "$SCRIPT_UNDER_TEST" \
    --skip-internal --skip-external --skip-build \
    --repo "$TMP_REPO"
  [ "$status" -eq 1 ]
  [[ "$output" == *"audit: FAILED"* ]]
  [[ "$output" == *"RUSTSEC-2024-0001"* ]]
}

@test "--help advertises --require-audit (Issue #619)" {
  run "$SCRIPT_UNDER_TEST" --help
  [ "$status" -eq 0 ]
  [[ "$output" == *"--require-audit"* ]]
}

@test "external stage: interlocked crates are retried as one grouped update (Issue #619)" {
  write_path_manifest
  write_interlocked_lock
  write_fake_cargo
  write_interlocked_dry_log
  use_ancient_publish_fixture
  FAKE_PRECISE_FAIL=1
  export FAKE_PRECISE_FAIL
  cat >"$TMP_REPO/group.sh" <<EOF
#!/usr/bin/env bash
cat >"$TMP_REPO/Cargo.lock" <<'LOCK'
version = 4

[[package]]
name = "js-sys"
version = "0.3.105"

[[package]]
name = "syn"
version = "2.0.119"

[[package]]
name = "wasm-bindgen"
version = "0.2.128"
LOCK
exit 0
EOF
  FAKE_GROUP_SCRIPT="$TMP_REPO/group.sh"
  export FAKE_GROUP_SCRIPT

  run "$SCRIPT_UNDER_TEST" \
    --skip-internal --skip-audit --skip-build \
    --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
  [[ "$output" == *"bump: js-sys -> 0.3.105"* ]]
  [[ "$output" == *"bump: wasm-bindgen -> 0.2.128"* ]]
  [[ "$output" == *"regrouped"* ]]
  [[ "$output" == *"external: 2 bumped, 0 deferred, 0 failed"* ]]
  grep -q 'version = "0.2.128"' "$TMP_REPO/Cargo.lock"
}

@test "external stage: a regrouped update that smuggles an unvetted version is reverted (Issue #619)" {
  write_path_manifest
  write_interlocked_lock
  write_fake_cargo
  write_interlocked_dry_log
  use_ancient_publish_fixture
  FAKE_PRECISE_FAIL=1
  export FAKE_PRECISE_FAIL
  # The grouped resolve also drags `syn` to a version the quarantine gate never
  # cleared — the whole retry must be rolled back.
  cat >"$TMP_REPO/group.sh" <<EOF
#!/usr/bin/env bash
cat >"$TMP_REPO/Cargo.lock" <<'LOCK'
version = 4

[[package]]
name = "js-sys"
version = "0.3.105"

[[package]]
name = "syn"
version = "3.0.5"

[[package]]
name = "wasm-bindgen"
version = "0.2.128"
LOCK
exit 0
EOF
  FAKE_GROUP_SCRIPT="$TMP_REPO/group.sh"
  export FAKE_GROUP_SCRIPT

  run "$SCRIPT_UNDER_TEST" \
    --skip-internal --skip-audit --skip-build \
    --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
  [[ "$output" == *"unvetted"* ]]
  [[ "$output" == *"external: 0 bumped, 0 deferred, 2 failed"* ]]
  # Lock restored to the pre-retry state.
  grep -q 'version = "2.0.119"' "$TMP_REPO/Cargo.lock"
  grep -q 'version = "0.2.127"' "$TMP_REPO/Cargo.lock"
}

@test "external stage: a failed regroup surfaces cargo's own error (Issue #619)" {
  write_path_manifest
  write_interlocked_lock
  write_fake_cargo
  write_interlocked_dry_log
  use_ancient_publish_fixture
  FAKE_PRECISE_FAIL=1
  export FAKE_PRECISE_FAIL
  cat >"$TMP_REPO/group.sh" <<'EOF'
#!/usr/bin/env bash
echo "error: failed to select a version for the requirement \`locked = \"=1.0.0\"\`" >&2
exit 101
EOF
  FAKE_GROUP_SCRIPT="$TMP_REPO/group.sh"
  export FAKE_GROUP_SCRIPT

  run "$SCRIPT_UNDER_TEST" \
    --skip-internal --skip-audit --skip-build \
    --repo "$TMP_REPO"
  [ "$status" -eq 0 ]
  [[ "$output" == *"fail: js-sys -> 0.3.105"* ]]
  [[ "$output" == *"failed to select a version"* ]]
  [[ "$output" == *"external: 0 bumped, 0 deferred, 2 failed"* ]]
}

@test "external stage: a failing cargo update --dry-run fails loud (Issue #619)" {
  write_path_manifest
  write_fake_cargo
  cat >"$TMP_REPO/dry.log" <<'EOF'
error: failed to load manifest for workspace member `rust_scorer`
EOF
  FAKE_DRY_LOG="$TMP_REPO/dry.log"
  FAKE_DRY_RC=101
  export FAKE_DRY_LOG FAKE_DRY_RC
  run "$SCRIPT_UNDER_TEST" \
    --skip-internal --skip-audit --skip-build \
    --repo "$TMP_REPO"
  [ "$status" -eq 1 ]
  [[ "$output" == *"failed to load manifest"* ]]
  [[ "$output" == *"dry-run"* ]]
}
