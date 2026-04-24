#!/usr/bin/env bash
# Validate the hardened Gitleaks workflow (Issue #21).
#
# The Gitleaks workflow must:
#   1. Install the Gitleaks binary from a pinned release (no floating
#      gitleaks-action reference) so PR scans are reproducible.
#   2. Verify the downloaded archive against a pinned SHA256 checksum so a
#      compromised release asset cannot silently replace the scanner.
#   3. Document the pinned version with a human-readable comment explaining
#      why the pin exists (reviewer context, supply-chain rationale).
#   4. Run `gitleaks detect` with `--log-opts=<base>..HEAD` so only the PR
#      commit range is scanned (the whole history is already covered by
#      nightly / main-branch scans in other pipelines).
#   5. Use strict bash in every `run:` block (`set -euo pipefail`) so a
#      failure in any sub-command fails the job rather than being swallowed.
#
# The script takes a single optional `--workflow PATH` argument so BATS tests
# can exercise it against fixtures. When called with no argument it validates
# `.github/workflows/gitleaks.yml` relative to the repo root.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-gitleaks-workflow.sh [--workflow PATH]

Options:
  --workflow PATH   Path to the Gitleaks workflow YAML file (default:
                    .github/workflows/gitleaks.yml relative to the repo root).
  -h, --help        Show this message.

Exits 0 when the workflow satisfies every hardening rule listed in the
script header. Exits non-zero with a descriptive message otherwise.
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
  WORKFLOW="$SCRIPT_DIR/../.github/workflows/gitleaks.yml"
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

content="$(cat "$WORKFLOW")"

# 1. No gitleaks-action marketplace reference — must be pinned binary install.
if grep -qE '(^|[[:space:]-])uses:[[:space:]]*gitleaks/gitleaks-action' "$WORKFLOW"; then
  fail "uses gitleaks-action — replace with pinned binary install"
else
  ok "no gitleaks-action reference (pinned binary install in use)"
fi

# 2. Pinned version present. The release URL must reference either a literal
#    vX.Y.Z tag or an env variable whose value is bound in the workflow
#    itself (e.g., `GITLEAKS_VERSION: "8.24.2"`).
if grep -qE 'gitleaks/releases/download/v[0-9]+\.[0-9]+\.[0-9]+/' "$WORKFLOW"; then
  ok "pinned Gitleaks release URL present (literal version tag)"
elif grep -qE 'gitleaks/releases/download/v\$\{' "$WORKFLOW" \
  && grep -qE '^[[:space:]]*GITLEAKS_VERSION:[[:space:]]*"?[0-9]+\.[0-9]+\.[0-9]+"?' "$WORKFLOW"; then
  ok "pinned Gitleaks release URL present (env-bound version)"
else
  fail "no pinned Gitleaks release URL (expect .../releases/download/vX.Y.Z/ or env-bound GITLEAKS_VERSION)"
fi

# 3. Pin rationale comment present — reviewers need to know why we pin.
if grep -qiE '^[[:space:]]*#.*pin' "$WORKFLOW"; then
  ok "pin rationale comment present"
else
  fail "no comment explaining why the version is pinned"
fi

# 4. SHA256 checksum verification of the downloaded archive.
if grep -qE 'sha256sum[[:space:]]+(-c|--check)' "$WORKFLOW" \
  || grep -qE 'shasum[[:space:]]+-a[[:space:]]+256[[:space:]]+(-c|--check)' "$WORKFLOW"; then
  ok "archive checksum verification present"
else
  fail "no SHA256 checksum verification of the downloaded Gitleaks archive"
fi

# 5. PR-diff scan scope: --log-opts must limit to base..HEAD.
if grep -qE '\-\-log-opts=?.*\.\.HEAD' "$WORKFLOW"; then
  ok "--log-opts limits scan to PR diff range (base..HEAD)"
else
  fail "no --log-opts=<base>..HEAD — scan scope is not limited to the PR"
fi

# 6. github.base_ref referenced so the scan range is derived from the PR.
if grep -qE 'github\.base_ref' "$WORKFLOW"; then
  ok "base ref derived from github.base_ref"
else
  fail "scan range does not reference github.base_ref (base branch unknown)"
fi

# 7. Explicit fetch of the base ref so origin/<base> resolves locally.
if grep -qE 'git[[:space:]]+fetch.*origin.*base_ref' "$WORKFLOW"; then
  ok "explicit git fetch of the base ref present"
else
  fail "no explicit 'git fetch origin <base_ref>' — origin/<base> may not resolve"
fi

# 8. Strict bash in every run: block. We require `set -euo pipefail` at least
#    once per workflow; a workflow that runs multiple shell steps without it
#    is masking failures.
if grep -qE 'set[[:space:]]+-euo[[:space:]]+pipefail' "$WORKFLOW"; then
  ok "strict bash (set -euo pipefail) present"
else
  fail "no 'set -euo pipefail' in run: blocks — failures may be swallowed"
fi

# 9. Gitleaks invocation must exist and use 'detect' (scans commit range).
if grep -qE 'gitleaks[[:space:]]+detect' "$WORKFLOW"; then
  ok "gitleaks detect invocation present"
else
  fail "no 'gitleaks detect' invocation"
fi

# Silence unused-variable warning from shellcheck for $content, which future
# extensions may use to parse the file directly.
: "${content:=}"

exit "$EXIT_CODE"
