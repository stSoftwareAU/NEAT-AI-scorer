#!/usr/bin/env bats
# Tests for scripts/build-pgo.sh — Issue #43.
#
# We don't actually run cargo / llvm-profdata here (a real PGO build cycle takes
# minutes). Instead we shim each external command on PATH and assert that:
#
#   * the instrumented build is invoked first with -Cprofile-generate,
#   * the instrumented binary is run twice (single-creature + directory mode),
#   * llvm-profdata merge is called against the profdata directory,
#   * the final cargo build is invoked with -Cprofile-use,
#   * the script exits non-zero when a step fails,
#   * env-var overrides (PGO_BYTES, LLVM_PROFDATA, PGO_PROFDATA_DIR) flow through.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/build-pgo.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_REPO="$(mktemp -d)"
  export TMP_REPO
  CARGO_LOG="${TMP_REPO}/cargo-log"
  PROFDATA_LOG="${TMP_REPO}/profdata-log"
  BIN_LOG="${TMP_REPO}/bin-log"
  export CARGO_LOG PROFDATA_LOG BIN_LOG

  mkdir -p "${TMP_REPO}/scripts" "${TMP_REPO}/target/pgo"
  cp "$SCRIPT_UNDER_TEST" "${TMP_REPO}/scripts/build-pgo.sh"

  TMP_BIN="${TMP_REPO}/.shim-bin"
  mkdir -p "$TMP_BIN"
  export TMP_BIN

  # Shim cargo: log args + RUSTFLAGS, and on the first call materialise an
  # executable rust_scorer that just records each invocation in BIN_LOG.
  cat >"${TMP_BIN}/cargo" <<EOF
#!/bin/bash
{
  echo "ARGS: \$*"
  echo "RUSTFLAGS=\${RUSTFLAGS:-unset}"
} >>"${CARGO_LOG}"
mkdir -p "${TMP_REPO}/target/pgo"
cat >"${TMP_REPO}/target/pgo/rust_scorer" <<'BIN_EOF'
#!/bin/bash
echo "RUN \$*" >>"${BIN_LOG}"
# Simulate the instrumented binary writing a *.profraw file.
if [ -n "\${LLVM_PROFILE_FILE:-}" ]; then
  : > "\${LLVM_PROFILE_FILE}"
elif [ -d "${TMP_REPO}/target/pgo-profiles" ]; then
  : > "${TMP_REPO}/target/pgo-profiles/run.profraw"
fi
BIN_EOF
chmod +x "${TMP_REPO}/target/pgo/rust_scorer"
exit "\${SHIM_CARGO_EXIT:-0}"
EOF
  chmod +x "${TMP_BIN}/cargo"

  # Shim llvm-profdata: log argv and create the merged file.
  cat >"${TMP_BIN}/llvm-profdata" <<EOF
#!/bin/bash
echo "PROFDATA: \$*" >>"${PROFDATA_LOG}"
out=""
prev=""
for a in "\$@"; do
  if [ "\$prev" = "-o" ]; then out="\$a"; fi
  prev="\$a"
done
if [ -n "\$out" ]; then
  echo "merged" > "\$out"
fi
exit "\${SHIM_PROFDATA_EXIT:-0}"
EOF
  chmod +x "${TMP_BIN}/llvm-profdata"

  PATH="${TMP_BIN}:$PATH"
  export PATH
  HOME_BACKUP="$HOME"
  HOME="$TMP_BIN"
  export HOME HOME_BACKUP
}

teardown() {
  rm -rf "$TMP_REPO"
  HOME="${HOME_BACKUP:-$HOME}"
}

@test "phase 1 builds an instrumented binary with -Cprofile-generate" {
  PGO_BYTES=4096 PGO_CREATURES=2 run "${TMP_REPO}/scripts/build-pgo.sh"
  [ "$status" -eq 0 ]
  # First cargo invocation must carry -Cprofile-generate.
  first_args="$(grep -m1 '^ARGS:' "$CARGO_LOG")"
  first_flags="$(grep -m1 '^RUSTFLAGS=' "$CARGO_LOG")"
  [[ "$first_args" == *"build --profile pgo -p rust_scorer --bin rust_scorer"* ]]
  [[ "$first_flags" == *"-Cprofile-generate="* ]]
}

@test "phase 4 builds the final binary with -Cprofile-use" {
  PGO_BYTES=4096 PGO_CREATURES=2 run "${TMP_REPO}/scripts/build-pgo.sh"
  [ "$status" -eq 0 ]
  # Second cargo invocation must carry -Cprofile-use.
  last_flags="$(grep '^RUSTFLAGS=' "$CARGO_LOG" | tail -n1)"
  [[ "$last_flags" == *"-Cprofile-use="* ]]
  [[ "$last_flags" == *"merged.profdata"* ]]
}

@test "instrumented binary is run for both single-creature and directory mode" {
  PGO_BYTES=4096 PGO_CREATURES=2 run "${TMP_REPO}/scripts/build-pgo.sh"
  [ "$status" -eq 0 ]
  [ "$(grep -c '^RUN ' "$BIN_LOG")" -eq 2 ]
  grep -q "creature.json" "$BIN_LOG"
  grep -q "creatures " "$BIN_LOG"
}

@test "llvm-profdata merge is invoked against the profdata directory" {
  PGO_BYTES=4096 PGO_CREATURES=2 run "${TMP_REPO}/scripts/build-pgo.sh"
  [ "$status" -eq 0 ]
  grep -q "PROFDATA: merge -o" "$PROFDATA_LOG"
  grep -q "merged.profdata" "$PROFDATA_LOG"
}

@test "non-zero cargo exit propagates" {
  SHIM_CARGO_EXIT=2 PGO_BYTES=4096 PGO_CREATURES=2 \
    run "${TMP_REPO}/scripts/build-pgo.sh"
  [ "$status" -ne 0 ]
}

@test "non-zero llvm-profdata exit propagates" {
  SHIM_PROFDATA_EXIT=3 PGO_BYTES=4096 PGO_CREATURES=2 \
    run "${TMP_REPO}/scripts/build-pgo.sh"
  [ "$status" -ne 0 ]
}

@test "missing llvm-profdata yields a clear error" {
  rm -f "${TMP_BIN}/llvm-profdata"
  # Force the auto-discovery to fail by pointing rustc to a sysroot with no
  # llvm-tools, via a minimal rustc shim on PATH.
  cat >"${TMP_BIN}/rustc" <<EOF
#!/bin/bash
[ "\$1" = "--print" ] && [ "\$2" = "sysroot" ] && echo "${TMP_REPO}/empty-sysroot" && exit 0
exit 0
EOF
  chmod +x "${TMP_BIN}/rustc"
  mkdir -p "${TMP_REPO}/empty-sysroot/lib/rustlib"

  PGO_BYTES=4096 PGO_CREATURES=2 LLVM_PROFDATA="" \
    run "${TMP_REPO}/scripts/build-pgo.sh"
  [ "$status" -ne 0 ]
  [[ "$output" == *"llvm-profdata"* ]]
}

@test "PGO_PROFDATA_DIR env override is honoured" {
  custom="${TMP_REPO}/custom-profdata"
  PGO_BYTES=4096 PGO_CREATURES=2 PGO_PROFDATA_DIR="$custom" \
    run "${TMP_REPO}/scripts/build-pgo.sh"
  [ "$status" -eq 0 ]
  grep -q "$custom" "$CARGO_LOG"
  grep -q "$custom" "$PROFDATA_LOG"
}
