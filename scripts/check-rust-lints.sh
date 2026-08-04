#!/usr/bin/env bash
# Validate crate-level rustc lint hardening for the rust_scorer workspace
# (Issue #274).
#
# The workspace configured only Clippy lints, leaving the rustc (`rust`) lint
# groups unenforced at the source-tree level. Relying solely on a CI
# `-D warnings` flag leaves the tree itself unhardened, so a local build or a
# differently-configured CI step would not catch a regression at the point it
# is introduced. This validator enforces that:
#   1. The root Cargo.toml declares a `[workspace.lints.rust]` table.
#   2. `unsafe_op_in_unsafe_fn` is denied (an unguarded unsafe op inside an
#      `unsafe fn` must not slip in silently — the crate uses `unsafe` in hot
#      paths).
#   3. `unused` is denied (dead code / unused imports must not reach Develop).
#   4. The posture is per-lint, NOT a blanket `#![deny(warnings)]` — a future
#      compiler warning must not break the build unexpectedly.
#   5. `missing_docs` is scoped to the library surface via
#      `#![warn(missing_docs)]` in `rust_scorer/src/lib.rs` (the doc-noisy
#      binary targets are intentionally not forced to document every item).
#
# The script takes optional `--manifest PATH` and `--lib PATH` arguments so
# BATS tests can exercise it against fixtures. With no argument it validates the
# real repository files relative to the repo root.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/check-harness.sh
source "$SCRIPT_DIR/lib/check-harness.sh"

usage() {
  cat <<'EOF'
Usage: check-rust-lints.sh [--manifest PATH] [--lib PATH]

Options:
  --manifest PATH   Path to the root Cargo.toml (default: Cargo.toml relative
                    to the repo root).
  --lib PATH        Path to the library crate root (default:
                    rust_scorer/src/lib.rs relative to the repo root).
  -h, --help        Show this message.

Exits 0 when the files satisfy every rule listed in the script header.
Exits non-zero with a descriptive message otherwise.
EOF
}

MANIFEST=""
LIB=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --manifest)
      check_flag_value --manifest $#
      MANIFEST="$2"
      shift 2
      ;;
    --lib)
      check_flag_value --lib $#
      LIB="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      check_unknown_arg "$1"
      ;;
  esac
done

[[ -n "$MANIFEST" ]] || MANIFEST="$(check_repo_path "Cargo.toml")"
[[ -n "$LIB" ]] || LIB="$(check_repo_path "rust_scorer/src/lib.rs")"

check_require_file "$MANIFEST" "Cargo.toml"
check_require_file "$LIB" "library root"

# Every message names its own file, so ok/fail carry no subject prefix.

# Extract the body of the [workspace.lints.rust] table: every line from the
# header up to (but not including) the next table header. Keeps rule checks
# scoped to the rust lint table and away from the clippy table.
rust_table="$(awk '
  /^\[workspace\.lints\.rust\]/ { grab = 1; next }
  /^\[/ { grab = 0 }
  grab { print }
' "$MANIFEST")"

# 1. The [workspace.lints.rust] table is present.
if grep -qE '^\[workspace\.lints\.rust\]' "$MANIFEST"; then
  ok "[workspace.lints.rust] table present"
else
  fail "missing [workspace.lints.rust] table in $MANIFEST"
fi

# 2. unsafe_op_in_unsafe_fn is denied.
if echo "$rust_table" | grep -qE '^[[:space:]]*unsafe_op_in_unsafe_fn[[:space:]]*=[[:space:]]*"deny"'; then
  ok "unsafe_op_in_unsafe_fn denied"
else
  fail "unsafe_op_in_unsafe_fn must be set to \"deny\" in [workspace.lints.rust]"
fi

# 3. unused is denied.
if echo "$rust_table" | grep -qE '^[[:space:]]*unused[[:space:]]*=[[:space:]]*"deny"'; then
  ok "unused denied"
else
  fail "unused must be set to \"deny\" in [workspace.lints.rust]"
fi

# 4. No blanket deny(warnings) — per-lint denies only.
if echo "$rust_table" | grep -qE '^[[:space:]]*warnings[[:space:]]*='; then
  fail "blanket 'warnings' lint set in [workspace.lints.rust] — prefer per-lint denies so a future compiler warning does not break the build"
else
  ok "no blanket warnings lint (per-lint denies preferred)"
fi

# 5. missing_docs is scoped to the library surface.
if grep -qE '^[[:space:]]*#!\[warn\(missing_docs\)\]' "$LIB"; then
  ok "missing_docs scoped to the library surface (#![warn(missing_docs)] in $LIB)"
else
  fail "missing_docs must be scoped to the library surface via #![warn(missing_docs)] in $LIB"
fi

exit "$EXIT_CODE"
