#!/usr/bin/env bash
# Validate the repository CODEOWNERS file (Issue #176).
#
# The repository ships privileged GitHub Actions workflows — `semgrep.yml`
# runs with a non-GITHUB_TOKEN secret (`SEMGREP_APP_TOKEN`). Without a
# CODEOWNERS rule covering `.github/workflows/`, a single account could
# self-approve a workflow edit that exfiltrates that secret or weakens a
# security gate. A CODEOWNERS rule over the workflow directory forces a
# designated maintainer's review on every such change.
#
# The CODEOWNERS file must:
#   1. Exist at one of the three GitHub-recognised paths (`CODEOWNERS`,
#      `.github/CODEOWNERS`, or `docs/CODEOWNERS`).
#   2. Contain at least one ownership rule (pattern + owner).
#   3. Contain a rule whose pattern covers `.github/workflows/` so workflow
#      changes always request an owner review.
#   4. Assign every rule at least one owner, and every owner token must be a
#      valid GitHub handle (`@user`), team (`@org/team`), or email address.
#
# The script takes a single optional `--codeowners PATH` argument so BATS
# tests can exercise it against fixtures. With no argument it locates the
# CODEOWNERS file at the first recognised path relative to the repo root.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-codeowners.sh [--codeowners PATH]

Options:
  --codeowners PATH   Path to the CODEOWNERS file to validate (default: the
                      first of CODEOWNERS, .github/CODEOWNERS, docs/CODEOWNERS
                      that exists relative to the repo root).
  -h, --help          Show this message.

Exits 0 when the CODEOWNERS file satisfies every rule in the script header.
Exits non-zero with a descriptive message otherwise.
EOF
}

CODEOWNERS=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --codeowners)
      CODEOWNERS="$2"
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

if [[ -z "$CODEOWNERS" ]]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  REPO_ROOT="$SCRIPT_DIR/.."
  for candidate in "CODEOWNERS" ".github/CODEOWNERS" "docs/CODEOWNERS"; do
    if [[ -f "$REPO_ROOT/$candidate" ]]; then
      CODEOWNERS="$REPO_ROOT/$candidate"
      break
    fi
  done
  if [[ -z "$CODEOWNERS" ]]; then
    echo "CODEOWNERS not found at any recognised path: CODEOWNERS, .github/CODEOWNERS, docs/CODEOWNERS" >&2
    exit 2
  fi
fi

if [[ ! -f "$CODEOWNERS" ]]; then
  echo "CODEOWNERS file not found: $CODEOWNERS" >&2
  exit 2
fi

EXIT_CODE=0
fail() {
  echo "FAIL $CODEOWNERS: $*" >&2
  EXIT_CODE=1
}
ok() {
  echo "OK   $CODEOWNERS: $*"
}

# Does a CODEOWNERS pattern cover .github/workflows/ ?
# CODEOWNERS uses gitignore-style globs; we accept the patterns that
# unambiguously include the workflow directory. The leading slash is optional.
covers_workflows() {
  local pat="${1#/}"
  case "$pat" in
    '*') return 0 ;;
    '.github' | '.github/' | '.github/*' | '.github/**') return 0 ;;
    '.github/workflows' | '.github/workflows/' | '.github/workflows/*' | '.github/workflows/**') return 0 ;;
    *) return 1 ;;
  esac
}

# Is a token a valid CODEOWNERS owner — @user, @org/team, or an email?
valid_owner() {
  local owner="$1"
  if [[ "$owner" =~ ^@[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)?$ ]]; then
    return 0
  fi
  if [[ "$owner" =~ ^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}$ ]]; then
    return 0
  fi
  return 1
}

rule_count=0
workflow_covered=0

while IFS= read -r raw || [[ -n "$raw" ]]; do
  # Strip a trailing comment, then trim surrounding whitespace.
  line="${raw%%#*}"
  line="${line#"${line%%[![:space:]]*}"}"
  line="${line%"${line##*[![:space:]]}"}"
  [[ -z "$line" ]] && continue

  # First field is the pattern; the remainder are owners.
  read -r pattern owners <<<"$line"
  rule_count=$((rule_count + 1))

  if [[ -z "$owners" ]]; then
    fail "rule for pattern '$pattern' has no owner — every rule needs at least one owner"
    continue
  fi

  for owner in $owners; do
    if ! valid_owner "$owner"; then
      fail "invalid owner token '$owner' for pattern '$pattern' — expected @user, @org/team, or an email"
    fi
  done

  if covers_workflows "$pattern"; then
    workflow_covered=1
  fi
done <"$CODEOWNERS"

# 2. At least one ownership rule.
if [[ "$rule_count" -gt 0 ]]; then
  ok "contains $rule_count ownership rule(s)"
else
  fail "no ownership rules found — file is empty or only comments"
fi

# 3. A rule covering .github/workflows/.
if [[ "$workflow_covered" -eq 1 ]]; then
  ok "a rule covers .github/workflows/ — workflow changes require an owner review"
else
  fail "no rule covers .github/workflows/ — privileged workflow edits could merge unreviewed"
fi

exit "$EXIT_CODE"
