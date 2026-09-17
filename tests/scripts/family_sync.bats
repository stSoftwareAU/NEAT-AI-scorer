#!/usr/bin/env bats
# Tests for scripts/family-sync.sh (Issues #629, #630 — canonical runlib.sh
# and family-pins.sh sync).
#
# The sync is driven against a local `--source` file, or a stub `curl` serving
# local fixtures, so the suite never touches the network; the real fetch branch
# is exercised through its precondition (no curl on PATH), which must fail
# non-zero rather than leave the copy half-refreshed.

load 'test_helper'

setup() {
  SYNC="${BATS_TEST_DIRNAME}/../../scripts/family-sync.sh"
  [ -x "$SYNC" ] || chmod +x "$SYNC"

  TMP_DIR="$(mktemp -d)"
  CANONICAL="${TMP_DIR}/canonical.sh"
  TARGET="${TMP_DIR}/runlib.sh"
  printf '#!/usr/bin/env bash\n# canonical\nrunlib_install() { :; }\n' >"$CANONICAL"
  printf '#!/usr/bin/env bash\n# stale local copy\nrunlib_install() { :; }\n' >"$TARGET"
  export TMP_DIR CANONICAL TARGET
}

teardown() {
  rm -rf "$TMP_DIR"
}

@test "an identical copy is reported in sync and left alone" {
  cp "$CANONICAL" "$TARGET"
  run "$SYNC" --source "$CANONICAL" --target "$TARGET"
  [ "$status" -eq 0 ]
  [[ "$output" == *"matches NEAT-AI-core Develop"* ]]
  run diff "$CANONICAL" "$TARGET"
  [ "$status" -eq 0 ]
}

@test "a stale copy is refreshed byte-for-byte and made executable" {
  run "$SYNC" --source "$CANONICAL" --target "$TARGET"
  [ "$status" -eq 0 ]
  [[ "$output" == *"refreshed"* ]]
  run cmp "$CANONICAL" "$TARGET"
  [ "$status" -eq 0 ]
  [ -x "$TARGET" ]
}

@test "--check reports a stale copy without writing it" {
  run "$SYNC" --check --source "$CANONICAL" --target "$TARGET"
  [ "$status" -eq 1 ]
  [[ "$output" == *"differs from NEAT-AI-core Develop"* ]]
  run grep -q "stale local copy" "$TARGET"
  [ "$status" -eq 0 ]
}

@test "--check exits 0 when the copy already matches" {
  cp "$CANONICAL" "$TARGET"
  run "$SYNC" --check --source "$CANONICAL" --target "$TARGET"
  [ "$status" -eq 0 ]
}

@test "a missing canonical source fails loud and keeps the copy" {
  run "$SYNC" --source "${TMP_DIR}/absent.sh" --target "$TARGET"
  [ "$status" -eq 2 ]
  [[ "$output" == *"canonical source not found"* ]]
  run grep -q "stale local copy" "$TARGET"
  [ "$status" -eq 0 ]
}

@test "an empty fetch never overwrites the copy" {
  : >"${TMP_DIR}/empty.sh"
  run "$SYNC" --source "${TMP_DIR}/empty.sh" --target "$TARGET"
  [ "$status" -eq 2 ]
  [[ "$output" == *"empty file"* ]]
  run grep -q "stale local copy" "$TARGET"
  [ "$status" -eq 0 ]
}

@test "an error page is refused rather than installed over the copy" {
  printf '<html><body>404</body></html>\n' >"${TMP_DIR}/error.html"
  run "$SYNC" --source "${TMP_DIR}/error.html" --target "$TARGET"
  [ "$status" -eq 2 ]
  [[ "$output" == *"not a bash script"* ]]
  run grep -q "stale local copy" "$TARGET"
  [ "$status" -eq 0 ]
}

@test "a bash script that is not runlib.sh is refused" {
  printf '#!/usr/bin/env bash\necho hello\n' >"${TMP_DIR}/other.sh"
  run "$SYNC" --source "${TMP_DIR}/other.sh" --target "$TARGET"
  [ "$status" -eq 2 ]
  [[ "$output" == *"no runlib_install"* ]]
}

@test "the fetch fails loud when curl is missing" {
  STUB_DIR="${TMP_DIR}/stub"
  mkdir -p "$STUB_DIR"
  for tool in bash mktemp cp rm head grep cmp chmod mkdir dirname cd sed; do
    real="$(command -v "$tool" || true)"
    [ -n "$real" ] || continue
    ln -sf "$real" "${STUB_DIR}/${tool}"
  done
  PATH="$STUB_DIR" run "$SYNC" --target "$TARGET"
  [ "$status" -eq 2 ]
  [[ "$output" == *"curl not found"* ]]
  run grep -q "stale local copy" "$TARGET"
  [ "$status" -eq 0 ]
}

@test "an unknown argument is refused with usage" {
  run "$SYNC" --wat
  [ "$status" -eq 2 ]
  [[ "$output" == *"Unknown argument"* ]]
}

@test "the committed scripts/runlib.sh passes the guards and syncs byte-for-byte" {
  RUNLIB="${BATS_TEST_DIRNAME}/../../scripts/runlib.sh"
  run "$SYNC" --source "$RUNLIB" --target "$TARGET"
  [ "$status" -eq 0 ]
  run cmp "$RUNLIB" "$TARGET"
  [ "$status" -eq 0 ]
  [ -x "$TARGET" ]
}

# --- family-pins.sh, the second canonical script (Issue #630) ---------------

@test "the committed scripts/family-pins.sh passes the guards and syncs byte-for-byte" {
  PINS="${BATS_TEST_DIRNAME}/../../scripts/family-pins.sh"
  PINS_TARGET="${TMP_DIR}/family-pins.sh"
  run "$SYNC" --source "$PINS" --target "$PINS_TARGET"
  [ "$status" -eq 0 ]
  [[ "$output" == *"refreshed"* ]]
  run cmp "$PINS" "$PINS_TARGET"
  [ "$status" -eq 0 ]
  [ -x "$PINS_TARGET" ]
}

@test "a bash script that is not family-pins.sh is refused" {
  printf '#!/usr/bin/env bash\necho hello\n' >"${TMP_DIR}/other.sh"
  run "$SYNC" --source "${TMP_DIR}/other.sh" --target "${TMP_DIR}/family-pins.sh"
  [ "$status" -eq 2 ]
  [[ "$output" == *"declares no family-pins"* ]]
  [ ! -f "${TMP_DIR}/family-pins.sh" ]
}

@test "--check reports a stale family-pins.sh without writing it" {
  PINS_TARGET="${TMP_DIR}/family-pins.sh"
  printf '#!/usr/bin/env bash\n# stale family-pins\n' >"$PINS_TARGET"
  run "$SYNC" --check --file family-pins.sh \
    --source "${BATS_TEST_DIRNAME}/../../scripts/family-pins.sh" --target "$PINS_TARGET"
  [ "$status" -eq 1 ]
  [[ "$output" == *"differs from NEAT-AI-core Develop"* ]]
  run grep -q "stale family-pins" "$PINS_TARGET"
  [ "$status" -eq 0 ]
}

@test "a script outside the canonical set is refused" {
  run "$SYNC" --file quality.sh
  [ "$status" -eq 2 ]
  [[ "$output" == *"not a canonical family script"* ]]
  [[ "$output" == *"runlib.sh"* ]]
  [[ "$output" == *"family-pins.sh"* ]]
}

@test "--source without a file selection is refused" {
  run "$SYNC" --source "$CANONICAL"
  [ "$status" -eq 2 ]
  [[ "$output" == *"--source needs --file"* ]]
}

# A default run (no --file/--target) refreshes EVERY canonical script. Driven
# against a stub `curl` that serves local fixtures from a temporary repo root,
# so the loop is exercised without the network and without touching this repo.
@test "a default run refreshes every canonical script" {
  FIXTURES="${TMP_DIR}/canonical"
  FAKE_REPO="${TMP_DIR}/repo"
  mkdir -p "$FIXTURES" "${FAKE_REPO}/scripts" "${TMP_DIR}/bin"
  printf '#!/usr/bin/env bash\n# canonical runlib\nrunlib_install() { :; }\n' >"${FIXTURES}/runlib.sh"
  printf '#!/usr/bin/env bash\n# canonical family-pins\n_fp_die() { echo "family-pins: $*"; }\n' >"${FIXTURES}/family-pins.sh"
  printf '#!/usr/bin/env bash\n# stale runlib\nrunlib_install() { :; }\n' >"${FAKE_REPO}/scripts/runlib.sh"
  printf '#!/usr/bin/env bash\n# stale family-pins\n_fp_die() { echo "family-pins: $*"; }\n' >"${FAKE_REPO}/scripts/family-pins.sh"
  cp "$SYNC" "${FAKE_REPO}/scripts/family-sync.sh"

  # Stub curl: copy the fixture named by the URL's last path element.
  cat >"${TMP_DIR}/bin/curl" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
out=""
url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    http*) url="$1"; shift ;;
    *) shift ;;
  esac
done
[ -n "$out" ] && [ -n "$url" ] || exit 22
cp "${FIXTURE_DIR}/${url##*/}" "$out"
STUB
  chmod +x "${TMP_DIR}/bin/curl"

  FIXTURE_DIR="$FIXTURES" PATH="${TMP_DIR}/bin:$PATH" run "${FAKE_REPO}/scripts/family-sync.sh"
  [ "$status" -eq 0 ]
  [[ "$output" == *"refreshed ${FAKE_REPO}/scripts/runlib.sh"* ]]
  [[ "$output" == *"refreshed ${FAKE_REPO}/scripts/family-pins.sh"* ]]
  run cmp "${FIXTURES}/runlib.sh" "${FAKE_REPO}/scripts/runlib.sh"
  [ "$status" -eq 0 ]
  run cmp "${FIXTURES}/family-pins.sh" "${FAKE_REPO}/scripts/family-pins.sh"
  [ "$status" -eq 0 ]
}

@test "--check on a default run reports every stale copy and exits 1" {
  FIXTURES="${TMP_DIR}/canonical"
  FAKE_REPO="${TMP_DIR}/repo"
  mkdir -p "$FIXTURES" "${FAKE_REPO}/scripts" "${TMP_DIR}/bin"
  printf '#!/usr/bin/env bash\n# canonical runlib\nrunlib_install() { :; }\n' >"${FIXTURES}/runlib.sh"
  printf '#!/usr/bin/env bash\n# canonical family-pins\n_fp_die() { echo "family-pins: $*"; }\n' >"${FIXTURES}/family-pins.sh"
  printf '#!/usr/bin/env bash\n# stale runlib\nrunlib_install() { :; }\n' >"${FAKE_REPO}/scripts/runlib.sh"
  cp "${FIXTURES}/family-pins.sh" "${FAKE_REPO}/scripts/family-pins.sh"
  cp "$SYNC" "${FAKE_REPO}/scripts/family-sync.sh"

  cat >"${TMP_DIR}/bin/curl" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
out=""
url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    http*) url="$1"; shift ;;
    *) shift ;;
  esac
done
[ -n "$out" ] && [ -n "$url" ] || exit 22
cp "${FIXTURE_DIR}/${url##*/}" "$out"
STUB
  chmod +x "${TMP_DIR}/bin/curl"

  FIXTURE_DIR="$FIXTURES" PATH="${TMP_DIR}/bin:$PATH" run "${FAKE_REPO}/scripts/family-sync.sh" --check
  [ "$status" -eq 1 ]
  [[ "$output" == *"scripts/runlib.sh differs from NEAT-AI-core Develop"* ]]
  # The already-current copy is still visited and reported in the same run.
  [[ "$output" == *"scripts/family-pins.sh matches NEAT-AI-core Develop"* ]]
  run grep -q "stale runlib" "${FAKE_REPO}/scripts/runlib.sh"
  [ "$status" -eq 0 ]
}
