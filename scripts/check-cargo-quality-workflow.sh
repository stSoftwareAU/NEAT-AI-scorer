#!/usr/bin/env bash
# Validate the standalone Cargo Quality (fmt + clippy) workflow (Issue #66).
#
# The cargo-quality workflow must:
#   1. Trigger on pull_request so every change is fmt + clippy checked.
#   2. Declare an explicit `permissions:` block (least privilege).
#   3. Pin `actions/checkout` to a numeric major version or a 40-char SHA
#      (Node 24 policy).
#   4. Install a Rust toolchain via `dtolnay/rust-toolchain` with the
#      `rustfmt` and `clippy` components.
#   5. Invoke `cargo fmt --check` (any flag form) so unformatted code fails CI.
#   6. Invoke `cargo clippy` with `-D warnings` so lints fail CI.
#   7. Use a pull_request `branches:` filter that matches milestone/<slug>
#      branches (Issue #392). GitHub's `*` glob does NOT cross `/`, so `["*"]`
#      silently skips milestone sub-issue PRs; require `**` or an explicit
#      `milestone/` glob so the gate runs on them.
#
# The script takes a single optional `--workflow PATH` argument so BATS tests
# can exercise it against fixtures. When called with no argument it validates
# `.github/workflows/cargo-quality.yml` relative to the repo root.
set -euo pipefail

# Shared workflow-validation helpers (Issue #511) — the `actions/checkout` pin
# rule lives in one place instead of six inline copies that drift apart.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKFLOW_CHECKS_LIB="$SCRIPT_DIR/lib/workflow-checks.sh"
if [[ ! -r "$WORKFLOW_CHECKS_LIB" ]]; then
  echo "Missing shared helper: $WORKFLOW_CHECKS_LIB" >&2
  exit 2
fi
# shellcheck source=lib/workflow-checks.sh disable=SC1091
source "$WORKFLOW_CHECKS_LIB"

usage() {
  cat <<'EOF'
Usage: check-cargo-quality-workflow.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the cargo-quality workflow YAML file (default:
                    .github/workflows/cargo-quality.yml relative to the repo
                    root).
  -h, --help        Show this message.

Exits 0 when the workflow satisfies every rule listed in the script header.
Exits non-zero with a descriptive message otherwise.
EOF
}

WORKFLOW=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --workflow)
      WORKFLOW="$2"
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

if [[ -z "$WORKFLOW" ]]; then
  WORKFLOW="$SCRIPT_DIR/../.github/workflows/cargo-quality.yml"
fi

if [[ ! -f "$WORKFLOW" ]]; then
  echo "Workflow file not found: $WORKFLOW" >&2
  exit 2
fi

EXIT_CODE=0
fail() {
  echo "FAIL $WORKFLOW: $*" >&2
  EXIT_CODE=1
}
ok() {
  echo "OK   $WORKFLOW: $*"
}

# 1. Triggered on pull_request events.
if grep -qE '^[[:space:]]+pull_request:' "$WORKFLOW" \
  || grep -qE '^on:[[:space:]]*\[?.*pull_request' "$WORKFLOW"; then
  ok "triggers on pull_request"
else
  fail "workflow is not triggered on pull_request"
fi

# 2. Explicit permissions block (least privilege).
if grep -qE '^permissions:[[:space:]]*$' "$WORKFLOW" \
  && grep -qE '^[[:space:]]+contents:[[:space:]]*read' "$WORKFLOW"; then
  ok "permissions block grants only contents: read"
else
  fail "no 'permissions: contents: read' block — least-privilege required"
fi

# 3. actions/checkout pinned to a numeric major (vN) or a 40-char SHA, via the
#    shared rule in scripts/lib/workflow-checks.sh (Issue #511). Branch refs
#    disallowed.
require_pinned_checkout "$WORKFLOW"

# 4. Rust toolchain provisioned via dtolnay/rust-toolchain with rustfmt +
#    clippy components.
if grep -qE 'uses:[[:space:]]*dtolnay/rust-toolchain@' "$WORKFLOW"; then
  ok "dtolnay/rust-toolchain present"
else
  fail "dtolnay/rust-toolchain missing — Rust toolchain not provisioned"
fi

if grep -qE 'rustfmt' "$WORKFLOW" && grep -qE 'clippy' "$WORKFLOW"; then
  ok "rustfmt and clippy components requested"
else
  fail "rustfmt and clippy components must both be requested on the toolchain"
fi

# 5. cargo fmt --check invoked (accept --check or -- --check forms).
if grep -qE 'cargo[[:space:]]+fmt[^#]*--check' "$WORKFLOW"; then
  ok "cargo fmt --check invoked"
else
  fail "cargo fmt --check is not invoked — formatting drift must fail CI"
fi

# 6. cargo clippy with -D warnings.
if grep -qE 'cargo[[:space:]]+clippy' "$WORKFLOW" \
  && grep -qE -- '-D[[:space:]]+warnings' "$WORKFLOW"; then
  ok "cargo clippy invoked with -D warnings"
else
  fail "cargo clippy must be invoked with '-D warnings' so lints fail CI"
fi

# 7. pull_request branch filter matches milestone/<slug> branches (Issue #392).
#    GitHub's `*` glob does not match across `/`, so a `["*"]` filter skips
#    milestone/<slug> PRs and the gate never runs on milestone sub-issue PRs.
#    Require `**` (matches any branch) or an explicit `milestone/` glob.
branches_filter="$(grep -E '^[[:space:]]*branches:' "$WORKFLOW" || true)"
if [[ -z "$branches_filter" ]]; then
  fail "no pull_request 'branches:' filter found — cannot verify milestone coverage"
elif echo "$branches_filter" | grep -qE '\*\*|milestone/'; then
  ok "pull_request branch filter matches milestone/<slug> branches"
else
  fail "pull_request branch filter does not match milestone/<slug> — '*' does not cross '/'; use '**' or add 'milestone/*'"
fi

exit "$EXIT_CODE"
