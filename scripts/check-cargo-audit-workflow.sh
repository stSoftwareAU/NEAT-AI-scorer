#!/usr/bin/env bash
# Validate the standalone Cargo Security Audit workflow (Issue #64).
#
# The cargo-audit workflow must:
#   1. Trigger on pull_request so every change is audited before merge.
#   2. Trigger on a cron schedule so freshly published advisories surface
#      even on quiet branches (the lockfile does not change but the
#      advisory database does).
#   3. Declare an explicit `permissions:` block (least privilege).
#   4. Pin `actions/checkout` to a numeric major version (Node 24 policy).
#   5. Install a Rust toolchain via `dtolnay/rust-toolchain` so cargo-audit
#      can be installed and run.
#   6. Invoke cargo-audit — either directly (`cargo audit` after a
#      `cargo install cargo-audit` step) or via the official RustSec
#      action (`rustsec/audit-check@vN`).
#   7. Cover milestone/<slug> branches in the pull_request filter so the gate
#      runs on milestone sub-issue PRs, not only top-level base branches
#      (Issue #391). A bare "*" glob does not match refs containing a slash.
#
# The script takes a single optional `--workflow PATH` argument so BATS tests
# can exercise it against fixtures. When called with no argument it validates
# `.github/workflows/cargo-audit.yml` relative to the repo root.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-cargo-audit-workflow.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the cargo-audit workflow YAML file (default:
                    .github/workflows/cargo-audit.yml relative to the repo root).
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
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  WORKFLOW="$SCRIPT_DIR/../.github/workflows/cargo-audit.yml"
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

# 2. Schedule trigger present (cron) — covers advisory-DB updates.
if grep -qE '^[[:space:]]+schedule:' "$WORKFLOW" \
  && grep -qE '^[[:space:]]+- cron:' "$WORKFLOW"; then
  ok "triggers on schedule (cron)"
else
  fail "no schedule (cron) trigger — weekly audit run required"
fi

# 3. Explicit permissions block (least privilege).
if grep -qE '^permissions:[[:space:]]*$' "$WORKFLOW" \
  && grep -qE '^[[:space:]]+contents:[[:space:]]*read' "$WORKFLOW"; then
  ok "permissions block grants only contents: read"
else
  fail "no 'permissions: contents: read' block — least-privilege required"
fi

# 4. actions/checkout pinned to a numeric major (vN) or a 40-char SHA.
# Branch refs (e.g. `@main`) disallowed. Issue #136 widened the regex from
# `v?[0-9]+` so SHAs starting with hex letters a–f are accepted too —
# rustsec/audit-check has been bumped to commit `858dc40…` and that lesson
# applies preemptively to actions/checkout here.
checkout_line="$(grep -nE 'uses:[[:space:]]*actions/checkout@' "$WORKFLOW" || true)"
if [[ -z "$checkout_line" ]]; then
  fail "actions/checkout step missing — workflow cannot fetch the repo"
elif echo "$checkout_line" | grep -qE 'actions/checkout@(v[0-9]+|[0-9a-f]{40})\b'; then
  ok "actions/checkout pinned to a numeric major or 40-char SHA"
else
  fail "actions/checkout is not pinned — branch refs disallowed"
fi

# 5. Rust toolchain provisioned via dtolnay/rust-toolchain.
if grep -qE 'uses:[[:space:]]*dtolnay/rust-toolchain@' "$WORKFLOW"; then
  ok "dtolnay/rust-toolchain present"
else
  fail "dtolnay/rust-toolchain missing — Rust toolchain not provisioned"
fi

# 6. cargo-audit invoked — either via the rustsec action OR a direct
#    `cargo audit` run (after `cargo install cargo-audit`).
if grep -qE 'uses:[[:space:]]*rustsec/audit-check@' "$WORKFLOW"; then
  ok "cargo audit invoked via rustsec/audit-check action"
elif grep -qE '^[[:space:]]+(- )?run:[[:space:]]*cargo audit' "$WORKFLOW" \
  || grep -qE 'cargo[[:space:]]+audit([[:space:]]|$)' "$WORKFLOW"; then
  ok "cargo audit invoked directly"
else
  fail "cargo audit is not invoked — neither rustsec/audit-check nor a 'cargo audit' run step found"
fi

# 7. pull_request branch filter must include milestone branches so the gate
#    runs on milestone/<slug> sub-issue PRs. A bare "*" glob only matches
#    single-level refs, so milestone/<slug> would otherwise skip the gate and
#    merge unchecked into the milestone branch (Issue #391). The token
#    `milestone/*` also matches the wider `milestone/**` glob.
if grep -qE 'milestone/\*' "$WORKFLOW"; then
  ok "pull_request branch filter covers milestone/* branches"
else
  fail "pull_request branch filter omits milestone/* — milestone PRs skip the gate"
fi

exit "$EXIT_CODE"
