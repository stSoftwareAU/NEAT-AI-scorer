#!/usr/bin/env bats
# Tests for scripts/spell-check.sh — Issue #22.
#
# Exercises the local codespell preflight against synthetic source trees so
# behaviour (exit codes, reported typos, ignore list) is verified end-to-end
# without touching the real repository content.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/spell-check.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  # Make the user-site codespell install visible when the shell has been
  # started without the standard PATH extension (macOS, CI).
  if [ -d "$HOME/Library/Python/3.13/bin" ]; then
    export PATH="$HOME/Library/Python/3.13/bin:$PATH"
  fi
  if [ -d "$HOME/.local/bin" ]; then
    export PATH="$HOME/.local/bin:$PATH"
  fi

  if ! command -v codespell >/dev/null 2>&1; then
    skip "codespell not installed"
  fi

  TMP_REPO="$(mktemp -d)"
  export TMP_REPO
  cp "${BATS_TEST_DIRNAME}/../../.codespellrc" "$TMP_REPO/.codespellrc"
}

teardown() {
  rm -rf "$TMP_REPO"
}

@test "passes on a clean tree" {
  cat >"$TMP_REPO/clean.md" <<'EOF'
# A clean document with correctly spelt words.
EOF
  run "$SCRIPT_UNDER_TEST" --root "$TMP_REPO"
  [ "$status" -eq 0 ]
  [[ "$output" == *"codespell: no typos found"* ]]
}

@test "fails on a genuine typo" {
  cat >"$TMP_REPO/typo.md" <<'EOF'
This sentance contains a real typo.
EOF
  run "$SCRIPT_UNDER_TEST" --root "$TMP_REPO"
  [ "$status" -ne 0 ]
  [[ "$output" == *"sentance"* ]]
}

@test "ignores curated domain terms (MAPE / mape / renderD)" {
  cat >"$TMP_REPO/loss.md" <<'EOF'
Mean Absolute Percentage Error (MAPE) uses lowercase mape internally.
The renderD node entry also appears in device listings.
EOF
  run "$SCRIPT_UNDER_TEST" --root "$TMP_REPO"
  [ "$status" -eq 0 ]
}

@test "scans hidden files and filenames" {
  # Hidden file with a typo should still be caught.
  cat >"$TMP_REPO/.hidden" <<'EOF'
recieve should be flagged.
EOF
  run "$SCRIPT_UNDER_TEST" --root "$TMP_REPO"
  [ "$status" -ne 0 ]
  [[ "$output" == *"recieve"* ]]
}

@test "skips the target/ build directory" {
  mkdir -p "$TMP_REPO/target/debug"
  cat >"$TMP_REPO/target/debug/noisy.txt" <<'EOF'
recieve teh occured — typos inside build artefacts must be skipped.
EOF
  run "$SCRIPT_UNDER_TEST" --root "$TMP_REPO"
  [ "$status" -eq 0 ]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "real repository passes the spell check" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
}
