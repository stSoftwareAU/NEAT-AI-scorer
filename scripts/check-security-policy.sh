#!/usr/bin/env bash
# Validate the repository security policy file (Issue #177).
#
# `SECURITY.md` is the canonical, GitHub-recognised place to declare how to
# report a vulnerability *privately*, the expected response time, and which
# versions are supported. Without it a reporter has no private channel and
# defaults to opening a public issue — disclosing the vulnerability before a
# fix exists. GitHub surfaces the file in the repository's Security tab.
#
# The security policy must:
#   1. Exist at one of the three GitHub-recognised paths (`SECURITY.md`,
#      `.github/SECURITY.md`, or `docs/SECURITY.md`).
#   2. Be non-empty (contain more than just whitespace).
#   3. Declare a private reporting route — GitHub private vulnerability
#      reporting and/or a contact email address.
#   4. State an expected acknowledgement / response time.
#   5. Include a supported-versions table.
#   6. Document an emergency dependency-bump procedure that points a responder
#      at the existing tooling (`bump-deps.sh`) for an out-of-band fix.
#
# The script takes a single optional `--security-policy PATH` argument so BATS
# tests can exercise it against fixtures. With no argument it locates the
# policy file at the first recognised path relative to the repo root.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-security-policy.sh [--security-policy PATH]

Options:
  --security-policy PATH  Path to the security policy file to validate
                          (default: the first of SECURITY.md, .github/SECURITY.md,
                          docs/SECURITY.md that exists relative to the repo root).
  -h, --help              Show this message.

Exits 0 when the security policy satisfies every rule in the script header.
Exits non-zero with a descriptive message otherwise.
EOF
}

POLICY=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --security-policy)
      POLICY="$2"
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

if [[ -z "$POLICY" ]]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  REPO_ROOT="$SCRIPT_DIR/.."
  for candidate in "SECURITY.md" ".github/SECURITY.md" "docs/SECURITY.md"; do
    if [[ -f "$REPO_ROOT/$candidate" ]]; then
      POLICY="$REPO_ROOT/$candidate"
      break
    fi
  done
  if [[ -z "$POLICY" ]]; then
    echo "SECURITY.md not found at any recognised path: SECURITY.md, .github/SECURITY.md, docs/SECURITY.md" >&2
    exit 2
  fi
fi

if [[ ! -f "$POLICY" ]]; then
  echo "Security policy file not found: $POLICY" >&2
  exit 2
fi

EXIT_CODE=0
fail() {
  echo "FAIL $POLICY: $*" >&2
  EXIT_CODE=1
}
ok() {
  echo "OK   $POLICY: $*"
}

content="$(cat "$POLICY")"

# 2. Non-empty (more than whitespace).
if [[ -n "${content//[[:space:]]/}" ]]; then
  ok "is non-empty"
else
  fail "file is empty or whitespace only"
fi

# 3. Private reporting route — GitHub private vulnerability reporting and/or a
#    contact email address.
reporting_route=0
if grep -Eiq 'report a vulnerability|private(ly)? (vulnerability )?report|security advisor' "$POLICY"; then
  reporting_route=1
fi
if grep -Eiq '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' "$POLICY"; then
  reporting_route=1
fi
if [[ "$reporting_route" -eq 1 ]]; then
  ok "declares a private reporting route (GitHub private reporting and/or email)"
else
  fail "no private reporting route — add GitHub private vulnerability reporting and/or a contact email"
fi

# 4. Expected acknowledgement / response time.
if grep -Eiq 'response|acknowledge|acknowledgement|business day|within [0-9]' "$POLICY"; then
  ok "states an expected acknowledgement / response time"
else
  fail "no response-time expectation — state how soon a reporter can expect acknowledgement"
fi

# 5. Supported-versions table.
supported_versions=0
if grep -Eiq 'supported version' "$POLICY"; then
  # A Markdown table has at least one row delimited by pipes.
  if grep -Eq '^\s*\|.*\|' "$POLICY"; then
    supported_versions=1
  fi
fi
if [[ "$supported_versions" -eq 1 ]]; then
  ok "includes a supported-versions table"
else
  fail "no supported-versions table — add a 'Supported versions' section with a Markdown table"
fi

# 6. Emergency dependency-bump procedure — points a responder at the existing
#    tooling so an out-of-band supply-chain fix does not have to be
#    reverse-engineered mid-incident.
emergency_bump=0
if grep -Eiq 'emergency|out-of-band|out of band|expedited' "$POLICY" \
  && grep -Eq 'bump-deps\.sh' "$POLICY"; then
  emergency_bump=1
fi
if [[ "$emergency_bump" -eq 1 ]]; then
  ok "documents an emergency dependency-bump procedure"
else
  fail "no emergency dependency-bump procedure — point a responder at bump-deps.sh for an out-of-band fix"
fi

exit "$EXIT_CODE"
