#!/usr/bin/env bats
# Tests for scripts/family-sync.sh (Issue #629 — canonical runlib.sh sync).
#
# The sync is driven against a local `--source` file so the suite never
# touches the network; the fetch branch is exercised through its precondition
# (no curl on PATH), which must fail non-zero rather than leave the copy
# half-refreshed.

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
