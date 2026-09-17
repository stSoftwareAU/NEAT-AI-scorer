#!/usr/bin/env bats
# Tests for scripts/family-pins.sh (Issue #630) — the canonical helper that
# moves this repository's `neat-core` git-tag pin to NEAT-AI-core's newest
# release.
#
# The script itself is a byte-identical copy of NEAT-AI-core Develop's (the
# copy contract is asserted in family_sync.bats); what these tests pin down is
# that it works against *this* repository's layout — the pin lives in the
# workspace member `rust_scorer/Cargo.toml`, not in the root manifest — and
# that a failure to resolve a remote fails the run loud.
#
# Every case drives the real script against a fixture checkout whose family
# remote is a real local bare git repository carrying real `v*` tags, reached
# through a `url.<base>.insteadOf` rewrite in a throwaway global git config, so
# `git ls-remote` never leaves the machine. `cargo` is a shim that records its
# invocations, which makes "ran cargo update for the moved pin" — and "ran no
# cargo command at all" — assertions rather than inferences.

setup() {
  REPO_ROOT="${BATS_TEST_DIRNAME}/../.."
  SCRIPT="${REPO_ROOT}/scripts/family-pins.sh"
  [ -x "$SCRIPT" ] || chmod +x "$SCRIPT"

  WORK="${BATS_TEST_TMPDIR}"
  REMOTES="${WORK}/remotes"
  CONSUMER="${WORK}/consumer"
  SHIM_DIR="${WORK}/shims"
  export CARGO_SHIM_LOG="${WORK}/cargo-invocations.log"
  mkdir -p "$REMOTES" "$SHIM_DIR" "${CONSUMER}/rust_scorer"

  # A throwaway global config rewrites every family URL onto the local bare
  # remotes, so the script's real `git ls-remote` never leaves the machine.
  export GIT_CONFIG_NOSYSTEM=1
  export GIT_CONFIG_GLOBAL="${WORK}/gitconfig"
  printf '[url "file://%s/"]\n\tinsteadOf = https://github.com/stSoftwareAU/\n' \
    "$REMOTES" >"$GIT_CONFIG_GLOBAL"

  cat >"${SHIM_DIR}/cargo" <<'SHIM'
#!/usr/bin/env bash
echo "$*" >>"$CARGO_SHIM_LOG"
if [ "${CARGO_SHIM_UPDATE_FAILS:-0}" = "1" ]; then
  echo "error: package ID specification did not match any packages" >&2
  exit 101
fi
exit 0
SHIM
  chmod +x "${SHIM_DIR}/cargo"
  export PATH="${SHIM_DIR}:${PATH}"
}

# make_remote <name> <tag>… — a bare repository at $REMOTES/<name> carrying
# exactly those tags, which is what the family URL resolves to under the
# insteadOf rewrite above.
make_remote() {
  local name="$1" src="${WORK}/sources/$1" tag
  shift
  mkdir -p "$src"
  printf 'fixture remote\n' >"$src/README.md"
  git init -q "$src"
  git -C "$src" add -A
  git -C "$src" -c user.email=fixture@example.com -c user.name=fixture \
    commit -q -m "fixture"
  for tag in "$@"; do
    git -C "$src" tag "$tag"
  done
  git clone -q --bare "$src" "${REMOTES}/${name}"
}

# Copy this repository's REAL manifests into the fixture checkout, so the pin
# under test is the shipped one rather than a hand-written imitation.
write_shipped_workspace() {
  cp "${REPO_ROOT}/Cargo.toml" "${CONSUMER}/Cargo.toml"
  cp "${REPO_ROOT}/rust_scorer/Cargo.toml" "${CONSUMER}/rust_scorer/Cargo.toml"
}

# The tag the shipped manifest is pinned to right now.
shipped_pin() {
  sed -n 's/.*stSoftwareAU\/NEAT-AI-core", tag = "\([^"]*\)".*/\1/p' \
    "${REPO_ROOT}/rust_scorer/Cargo.toml"
}

run_pins() {
  run bash -c "cd '$CONSUMER' && '$SCRIPT'"
}

@test "the shipped rust_scorer pin is discovered in the workspace member and moved" {
  write_shipped_workspace
  make_remote NEAT-AI-core "$(shipped_pin)" v99.0.0
  run_pins
  [ "$status" -eq 0 ]
  [[ "$output" == *"neat-core $(shipped_pin) → v99.0.0"* ]]
  [[ "$output" == *"rust_scorer/Cargo.toml"* ]]
  grep -q 'tag = "v99.0.0"' "${CONSUMER}/rust_scorer/Cargo.toml"
  grep -q 'git = "https://github.com/stSoftwareAU/NEAT-AI-core"' \
    "${CONSUMER}/rust_scorer/Cargo.toml"
  # Cargo.lock follows the moved pin.
  grep -q '^update --package neat-core$' "$CARGO_SHIM_LOG"
}

@test "a pin already on the newest release is left alone and runs no cargo command" {
  write_shipped_workspace
  make_remote NEAT-AI-core v0.1.0 "$(shipped_pin)"
  run_pins
  [ "$status" -eq 0 ]
  run cmp "${REPO_ROOT}/rust_scorer/Cargo.toml" "${CONSUMER}/rust_scorer/Cargo.toml"
  [ "$status" -eq 0 ]
  [ ! -f "$CARGO_SHIM_LOG" ]
}

@test "a pre-release is never what the pin is moved onto" {
  write_shipped_workspace
  make_remote NEAT-AI-core "$(shipped_pin)" v99.0.0 v99.1.0-rc1
  run_pins
  [ "$status" -eq 0 ]
  grep -q 'tag = "v99.0.0"' "${CONSUMER}/rust_scorer/Cargo.toml"
}

@test "a remote that cannot be listed fails the run non-zero" {
  write_shipped_workspace
  # No fixture remote at all: `git ls-remote` cannot resolve the family URL.
  run_pins
  [ "$status" -eq 1 ]
  [[ "$output" == *"cannot list the tags of"* ]]
  # The manifest is left untouched rather than half-rewritten.
  run cmp "${REPO_ROOT}/rust_scorer/Cargo.toml" "${CONSUMER}/rust_scorer/Cargo.toml"
  [ "$status" -eq 0 ]
}

@test "a remote carrying no release tag fails the run non-zero" {
  write_shipped_workspace
  make_remote NEAT-AI-core v99.0.0-rc1
  run_pins
  [ "$status" -eq 1 ]
  [[ "$output" == *"no released"* ]]
}

@test "a failed cargo update fails the run loud" {
  write_shipped_workspace
  make_remote NEAT-AI-core "$(shipped_pin)" v99.0.0
  CARGO_SHIM_UPDATE_FAILS=1 run_pins
  [ "$status" -eq 1 ]
  [[ "$output" == *"cargo update --package neat-core failed"* ]]
}

@test "the run is idempotent — a second pass reports nothing to move" {
  write_shipped_workspace
  make_remote NEAT-AI-core "$(shipped_pin)" v99.0.0
  run_pins
  [ "$status" -eq 0 ]
  cp "${CONSUMER}/rust_scorer/Cargo.toml" "${WORK}/after-first-pass.toml"
  rm -f "$CARGO_SHIM_LOG"
  run_pins
  [ "$status" -eq 0 ]
  run cmp "${WORK}/after-first-pass.toml" "${CONSUMER}/rust_scorer/Cargo.toml"
  [ "$status" -eq 0 ]
  [ ! -f "$CARGO_SHIM_LOG" ]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}
