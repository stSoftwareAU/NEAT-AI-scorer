#!/usr/bin/env bash
# Validate the SBOM (Software Bill of Materials) workflow (Issue #172).
#
# `rust_scorer` ships a binary, so its dependency inventory should be
# exportable in a standard, scanner-consumable format. `Cargo.lock` already
# pins the full graph; this workflow exports it as a CycloneDX SBOM and
# uploads the document as a build artefact so downstream consumers can answer
# "are we affected by advisory X, and where?" without a Rust toolchain.
#
# The SBOM workflow must:
#   1. Trigger on a build event — pull_request, push, release, or
#      workflow_dispatch — so the inventory is refreshed when the graph
#      changes.
#   2. Declare an explicit `permissions:` block (least privilege:
#      contents: read).
#   3. Pin `actions/checkout` to a numeric major version (vN) or a 40-char SHA.
#   4. Install a Rust toolchain via `dtolnay/rust-toolchain` so cargo-cyclonedx
#      can resolve the workspace metadata.
#   5. Generate a CycloneDX SBOM via `cargo cyclonedx`.
#   6. Upload the generated SBOM as a build artefact (actions/upload-artifact).
#
# The script takes a single optional `--workflow PATH` argument so BATS tests
# can exercise it against fixtures. When called with no argument it validates
# `.github/workflows/sbom.yml` relative to the repo root.
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
Usage: check-sbom-workflow.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the SBOM workflow YAML file (default:
                    .github/workflows/sbom.yml relative to the repo root).
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
  WORKFLOW="$SCRIPT_DIR/../.github/workflows/sbom.yml"
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

# 1. Triggered on a build event — pull_request, push, release, or
#    workflow_dispatch.
if grep -qE '^[[:space:]]+(pull_request|push|release|workflow_dispatch):' "$WORKFLOW" \
  || grep -qE '^on:[[:space:]]*\[?.*(pull_request|push|release|workflow_dispatch)' "$WORKFLOW"; then
  ok "triggers on a build event"
else
  fail "no build trigger — pull_request, push, release or workflow_dispatch required"
fi

# 2. Explicit permissions block (least privilege).
if grep -qE '^permissions:[[:space:]]*$' "$WORKFLOW" \
  && grep -qE '^[[:space:]]+contents:[[:space:]]*read' "$WORKFLOW"; then
  ok "permissions block grants only contents: read"
else
  fail "no 'permissions: contents: read' block — least-privilege required"
fi

# 3. actions/checkout pinned to a numeric major (vN) or a 40-char SHA, via the
#    shared rule in scripts/lib/workflow-checks.sh (Issue #511).
require_pinned_checkout "$WORKFLOW"

# 4. Rust toolchain provisioned via dtolnay/rust-toolchain.
if grep -qE 'uses:[[:space:]]*dtolnay/rust-toolchain@' "$WORKFLOW"; then
  ok "dtolnay/rust-toolchain present"
else
  fail "dtolnay/rust-toolchain missing — Rust toolchain not provisioned"
fi

# 5. CycloneDX SBOM generated via cargo cyclonedx.
if grep -qE 'cargo[[:space:]-]+cyclonedx' "$WORKFLOW"; then
  ok "CycloneDX SBOM generated via cargo cyclonedx"
else
  fail "CycloneDX SBOM is not generated — a 'cargo cyclonedx' step is required"
fi

# 6. Generated SBOM uploaded as a build artefact.
if grep -qE 'uses:[[:space:]]*actions/upload-artifact@' "$WORKFLOW"; then
  ok "SBOM uploaded as an artefact via actions/upload-artifact"
else
  fail "SBOM is not uploaded — an actions/upload-artifact step is required"
fi

# 7. The self checkout (current repository — a checkout with no `repository:`
#    input) must set `persist-credentials: false` (Issue #385). This job only
#    reads the checked-out code to build the SBOM, so the workflow GITHUB_TOKEN
#    must not be persisted to `.git/config` where a later step could read it.
#    A cross-repo checkout (one with `repository:`, e.g. NEAT-AI-core) is
#    exempt — it legitimately needs a credential to fetch a different repo.
read -r self_seen self_ok < <(awk '
  BEGIN { self_seen = 0; self_ok = 0 }
  function flush() {
    if (in_checkout && !has_repo) {
      self_seen = 1
      if (has_persist) self_ok = 1
    }
  }
  /^[[:space:]]*-[[:space:]]/ {
    flush()
    in_checkout = 0; has_repo = 0; has_persist = 0
  }
  /uses:[[:space:]]*actions\/checkout@/ { in_checkout = 1 }
  /^[[:space:]]*repository:/            { if (in_checkout) has_repo = 1 }
  /persist-credentials:[[:space:]]*false/ { if (in_checkout) has_persist = 1 }
  END { flush(); print self_seen "\t" self_ok }
' "$WORKFLOW")

if [[ "$self_seen" != "1" ]]; then
  ok "no self checkout of the current repository to harden"
elif [[ "$self_ok" == "1" ]]; then
  ok "self checkout sets persist-credentials: false"
else
  fail "self checkout of the current repo must set 'persist-credentials: false' (Issue #385)"
fi

exit "$EXIT_CODE"
