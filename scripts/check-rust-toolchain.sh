#!/usr/bin/env bash
# Validate the pinned rust-toolchain.toml for reproducible builds (Issue #209).
#
# The repository SHA-pins every GitHub Action and container digest, but the Rust
# compiler version otherwise floats. A root `rust-toolchain.toml` makes `rustup`
# resolve the same compiler for local `./quality.sh` and CI. This validator
# enforces that the file:
#   1. Exists at the repository root.
#   2. Declares a `[toolchain]` table.
#   3. Pins a CONCRETE stable version (`X.Y.Z`) — never a floating channel
#      (`stable`, `beta`, `nightly`) which would defeat reproducibility.
#   4. Lists both the `rustfmt` and `clippy` components (the gate needs them).
#
# The script takes a single optional `--toolchain PATH` argument so BATS tests
# can exercise it against fixtures. With no argument it validates
# `rust-toolchain.toml` relative to the repo root.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-rust-toolchain.sh [--toolchain PATH]

Options:
  --toolchain PATH   Path to the rust-toolchain.toml file (default:
                     rust-toolchain.toml relative to the repo root).
  -h, --help         Show this message.

Exits 0 when the file satisfies every rule listed in the script header.
Exits non-zero with a descriptive message otherwise.
EOF
}

TOOLCHAIN=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --toolchain)
      TOOLCHAIN="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$TOOLCHAIN" ]]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  TOOLCHAIN="$SCRIPT_DIR/../rust-toolchain.toml"
fi

if [[ ! -f "$TOOLCHAIN" ]]; then
  echo "rust-toolchain.toml not found: $TOOLCHAIN" >&2
  exit 2
fi

EXIT_CODE=0
fail() {
  echo "FAIL $TOOLCHAIN: $*" >&2
  EXIT_CODE=1
}
ok() {
  echo "OK   $TOOLCHAIN: $*"
}

# 1. A [toolchain] table is present.
if grep -qE '^\[toolchain\]' "$TOOLCHAIN"; then
  ok "[toolchain] table present"
else
  fail "missing [toolchain] table"
fi

# 2. channel pins a concrete X.Y.Z version, not a floating channel.
channel_line="$(grep -E '^[[:space:]]*channel[[:space:]]*=' "$TOOLCHAIN" || true)"
if [[ -z "$channel_line" ]]; then
  fail "no 'channel' key — the compiler version is not pinned"
elif echo "$channel_line" | grep -qE 'channel[[:space:]]*=[[:space:]]*"[0-9]+\.[0-9]+\.[0-9]+"'; then
  ok "channel pinned to a concrete X.Y.Z version"
else
  fail "channel must pin a concrete X.Y.Z version (not stable/beta/nightly)"
fi

# 3. Both rustfmt and clippy components are listed.
components_line="$(grep -E '^[[:space:]]*components[[:space:]]*=' "$TOOLCHAIN" || true)"
if [[ -z "$components_line" ]]; then
  fail "no 'components' key — rustfmt and clippy must be declared"
elif echo "$components_line" | grep -q 'rustfmt' \
  && echo "$components_line" | grep -q 'clippy'; then
  ok "rustfmt and clippy components declared"
else
  fail "components must include both rustfmt and clippy"
fi

exit "$EXIT_CODE"
