#!/usr/bin/env bash
# Hermetic tests for the canonical scripts/runlib.sh (Issue #629).
#
# `scripts/runlib.sh` is a byte-identical copy of NEAT-AI-core's Develop copy
# and is never edited here (see `scripts/family-sync.sh`). These tests pin the
# behaviour this repository depends on: the `rust_scorer` bin — never a bench
# bin — installed at `~/.cargo/bin/rust_scorer` beside `.rust_scorer.version`,
# `target/` removed after a success and kept after a failure.
#
# No real cargo build ever runs. Fixture repositories are driven with a cargo
# shim that records its argv and fails loud on an unexpected invocation.
#
# Cross-platform: macOS bash 3.2, Ubuntu, AWS Linux.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNLIB="${SCRIPT_DIR}/runlib.sh"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "${WORK_DIR}"' EXIT
REAL_PATH="${PATH}"
# The fixtures carry their own HOME; a CARGO_HOME inherited from the caller
# would silently redirect every install out of it.
unset CARGO_HOME

PASSED=0
FAILED=0

if [[ ! -x "${RUNLIB}" ]]; then
  echo "FAIL: runlib not found or not executable: ${RUNLIB}" >&2
  exit 2
fi

assert_eq() {
  local desc="$1" expected="$2" actual="$3"
  if [[ "${expected}" == "${actual}" ]]; then
    echo "  PASS: ${desc}"
    PASSED=$((PASSED + 1))
  else
    echo "  FAIL: ${desc}"
    echo "    expected: '${expected}'"
    echo "    actual:   '${actual}'"
    FAILED=$((FAILED + 1))
  fi
}

assert_contains() {
  local desc="$1" needle="$2" file="$3"
  if grep -qF -- "${needle}" "${file}"; then
    echo "  PASS: ${desc}"
    PASSED=$((PASSED + 1))
  else
    echo "  FAIL: ${desc}"
    echo "    '${needle}' not found in ${file}:"
    sed 's/^/      /' "${file}"
    FAILED=$((FAILED + 1))
  fi
}

assert_absent() {
  local desc="$1" needle="$2" file="$3"
  if grep -qF -- "${needle}" "${file}"; then
    echo "  FAIL: ${desc}"
    echo "    unexpected '${needle}' in ${file}:"
    sed 's/^/      /' "${file}"
    FAILED=$((FAILED + 1))
  else
    echo "  PASS: ${desc}"
    PASSED=$((PASSED + 1))
  fi
}

# A workspace whose single member is `rust_scorer`. `$2` is the extra
# `[[bin]]` block set: "single" ships only the crate-named binary, "benches"
# mirrors this repository, where four bench binaries sit beside it.
make_fixture() {
  local root="$1" shape="$2"
  mkdir -p "${root}/rust_scorer/src/bin"
  cat >"${root}/Cargo.toml" <<'EOF'
[workspace]
members = ["rust_scorer"]
resolver = "2"
EOF
  cat >"${root}/rust_scorer/Cargo.toml" <<'EOF'
[package]
name = "rust_scorer"
version = "9.9.9"
edition = "2021"

[lib]
name = "rust_scorer"
path = "src/lib.rs"

[[bin]]
name = "rust_scorer"
path = "src/main.rs"
EOF
  if [[ "${shape}" == "benches" ]]; then
    local bench
    for bench in float_scan_bench cost_scan_bench gpu_pipeline_alloc_bench if_tree_batch_bench; do
      cat >>"${root}/rust_scorer/Cargo.toml" <<EOF

[[bin]]
name = "${bench}"
path = "src/bin/${bench}.rs"
EOF
      : >"${root}/rust_scorer/src/bin/${bench}.rs"
    done
  fi
  : >"${root}/rust_scorer/src/lib.rs"
  : >"${root}/rust_scorer/src/main.rs"
}

# A cargo shim that answers `metadata` for the fixture and, for `build`,
# behaves as `$2` says: "ok" writes the release binaries, "fail" exits 1, and
# "refuse" exits 99 so a build that should never have happened is loud.
install_cargo_shim() {
  local bin_dir="$1" build_mode="$2" fixture="$3" shape="$4"
  local bench_targets=""
  if [[ "${shape}" == "benches" ]]; then
    bench_targets=',{"kind":["bin"],"name":"float_scan_bench"}
      ,{"kind":["bin"],"name":"cost_scan_bench"}
      ,{"kind":["bin"],"name":"gpu_pipeline_alloc_bench"}
      ,{"kind":["bin"],"name":"if_tree_batch_bench"}'
  fi
  mkdir -p "${bin_dir}"
  cat >"${bin_dir}/cargo" <<EOF
#!/usr/bin/env bash
set -euo pipefail
printf 'cargo %s\n' "\$*" >>"${WORK_DIR}/cargo.log"
if [[ "\${1:-}" == "metadata" ]]; then
  cat <<'JSON'
{"target_directory":"${fixture}/target",
 "packages":[{"name":"rust_scorer","version":"9.9.9",
   "manifest_path":"${fixture}/rust_scorer/Cargo.toml",
   "targets":[{"kind":["lib"],"name":"rust_scorer"},
     {"kind":["bin"],"name":"rust_scorer"}${bench_targets}]}]}
JSON
  exit 0
fi
if [[ "\${1:-}" == "build" ]]; then
  case "${build_mode}" in
    ok)
      mkdir -p "${fixture}/target/release"
      for target in rust_scorer float_scan_bench; do
        printf '#!/bin/sh\necho scorer\n' >"${fixture}/target/release/\${target}"
        chmod +x "${fixture}/target/release/\${target}"
      done
      exit 0
      ;;
    fail)
      echo "cargo shim: build refused" >&2
      exit 1
      ;;
    *)
      echo "UNEXPECTED cargo: \$*" >&2
      exit 99
      ;;
  esac
fi
echo "UNEXPECTED cargo: \$*" >&2
exit 99
EOF
  chmod +x "${bin_dir}/cargo"

  # rustc is only ever asked its version and its host triple; the fixtures
  # declare no `rust-version`, so the toolchain gate stops there.
  cat >"${bin_dir}/rustc" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  --version) echo "rustc 9.9.9 (test shim)" ;;
  -vV) printf 'rustc 9.9.9 (test shim)\nhost: x86_64-unknown-linux-gnu\n' ;;
  *) echo "UNEXPECTED rustc: $*" >&2; exit 99 ;;
esac
EOF
  chmod +x "${bin_dir}/rustc"
}

# A fresh fixture + shim pair. Sets FIXTURE, HOME, PATH and truncates the
# cargo log so each case reads only its own invocations.
start_case() {
  local name="$1" shape="$2" build_mode="$3"
  FIXTURE="${WORK_DIR}/${name}"
  rm -rf "${FIXTURE}"
  make_fixture "${FIXTURE}" "${shape}"
  HOME="${WORK_DIR}/home-${name}"
  export HOME
  mkdir -p "${HOME}/.cargo/bin"
  install_cargo_shim "${WORK_DIR}/shim-${name}" "${build_mode}" "${FIXTURE}" "${shape}"
  PATH="${WORK_DIR}/shim-${name}:${REAL_PATH}"
  export PATH
  : >"${WORK_DIR}/cargo.log"
}

# Pretend a previous run installed version $1.
pretend_installed() {
  printf '#!/bin/sh\necho old\n' >"${HOME}/.cargo/bin/rust_scorer"
  chmod +x "${HOME}/.cargo/bin/rust_scorer"
  printf '%s\n' "$1" >"${HOME}/.cargo/bin/.rust_scorer.version"
}

echo "=== already installed, single-bin crate: no cargo command at all ==="
start_case single single refuse
pretend_installed 9.9.9
OUT="$(cd "${FIXTURE}" && bash "${RUNLIB}" 2>"${WORK_DIR}/single.err")" && RC=0 || RC=$?
assert_eq "already-installed exits 0" "0" "${RC}"
assert_eq "stdout is the installed CLI path" "${HOME}/.cargo/bin/rust_scorer" "${OUT}"
assert_contains "names the version on stderr" \
  "[rust_scorer] already installed v9.9.9" "${WORK_DIR}/single.err"
assert_eq "no cargo command ran" "" "$(cat "${WORK_DIR}/cargo.log")"

echo ""
echo "=== already installed, bench bins beside the CLI: no cargo build ==="
start_case benches benches refuse
pretend_installed 9.9.9
OUT="$(cd "${FIXTURE}" && bash "${RUNLIB}" 2>"${WORK_DIR}/benches.err")" && RC=0 || RC=$?
assert_eq "already-installed exits 0" "0" "${RC}"
assert_eq "stdout is the installed CLI path" "${HOME}/.cargo/bin/rust_scorer" "${OUT}"
assert_contains "names the version on stderr" \
  "[rust_scorer] already installed v9.9.9" "${WORK_DIR}/benches.err"
assert_absent "no cargo build ran" "cargo build" "${WORK_DIR}/cargo.log"

echo ""
echo "=== stale stamp: builds the rust_scorer bin, installs it, removes target/ ==="
start_case install benches ok
pretend_installed 1.0.0
OUT="$(cd "${FIXTURE}" && bash "${RUNLIB}" 2>"${WORK_DIR}/install.err")" && RC=0 || RC=$?
assert_eq "install exits 0" "0" "${RC}"
assert_eq "stdout is the installed CLI path" "${HOME}/.cargo/bin/rust_scorer" "${OUT}"
assert_eq "the CLI is installed and executable" "yes" \
  "$([[ -x "${HOME}/.cargo/bin/rust_scorer" ]] && echo yes || echo no)"
assert_eq "the stamp carries the crate version" "9.9.9" \
  "$(cat "${HOME}/.cargo/bin/.rust_scorer.version")"
assert_eq "target/ is removed after a successful install" "gone" \
  "$([[ -d "${FIXTURE}/target" ]] && echo present || echo gone)"
assert_contains "the removal names the path freed" "${FIXTURE}/target" "${WORK_DIR}/install.err"
assert_contains "the build selects the crate bin" \
  "cargo build --release --package rust_scorer --bin rust_scorer" "${WORK_DIR}/cargo.log"
assert_absent "no bench bin is built" "float_scan_bench" "${WORK_DIR}/cargo.log"
assert_eq "no bench bin is installed" "gone" \
  "$([[ -e "${HOME}/.cargo/bin/float_scan_bench" ]] && echo present || echo gone)"

echo ""
echo "=== failed build: keeps target/ and the previously installed CLI ==="
start_case failure benches fail
pretend_installed 1.0.0
mkdir -p "${FIXTURE}/target/release"
: >"${FIXTURE}/target/release/keep-me"
OUT="$(cd "${FIXTURE}" && bash "${RUNLIB}" 2>"${WORK_DIR}/failure.err")" && RC=0 || RC=$?
assert_eq "a failed build exits non-zero" "1" "${RC}"
assert_eq "stdout stays empty" "" "${OUT}"
assert_eq "target/ is kept for the next attempt" "present" \
  "$([[ -f "${FIXTURE}/target/release/keep-me" ]] && echo present || echo gone)"
assert_eq "the previous stamp is untouched" "1.0.0" \
  "$(cat "${HOME}/.cargo/bin/.rust_scorer.version")"
assert_eq "the previous CLI is untouched" "old" \
  "$(sh "${HOME}/.cargo/bin/rust_scorer")"

echo ""
echo "=== summary: ${PASSED} passed, ${FAILED} failed ==="
[[ "${FAILED}" -eq 0 ]]
