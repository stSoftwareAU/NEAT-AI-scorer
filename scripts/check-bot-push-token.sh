#!/usr/bin/env bash
# Validate that bot-push PR workflows authenticate with a short-lived,
# repo-scoped installation token (Issues #435, #498).
#
# Auto Format and Version Increment commit back to the PR branch. A push
# authenticated only with GITHUB_TOKEN attributes the resulting
# `synchronize` event to github-actions[bot], and GitHub holds the
# follow-on required checks behind "N checks awaiting approval /
# Approve and run" (Issue #435).
#
# The original fix pushed with the organisation-level ACTIONS_PUSH PAT. That
# PAT is long-lived and org-scoped, so anything that reaches it steps up from
# single-repo write access to the PAT's full scope — including pushing to
# other organisation repositories. Issue #498 replaces it with a token minted
# per run by a GitHub App: `contents: write` on **this repository only**, and
# expiring within the hour. The PAT stays only as a fallback until an org
# admin creates the App and stores its secrets.
#
# Rules enforced on each guarded workflow:
#   1. It mints the push credential with `actions/create-github-app-token`,
#      SHA-pinned per the supply-chain policy (Issue #100).
#   2. The mint step narrows the token to `permission-contents: write`.
#   3. The mint step scopes it to this repository via `repositories:` and does
#      not set `owner:`, which would widen the token across the organisation.
#   4. Every `GH_PAT` binding prefers that minted token, falling back to
#      `secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN` so pushes keep working
#      (and keep clearing the Issue #435 approval gate) before the App exists.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-bot-push-token.sh [--workflow PATH]

Options:
  --workflow PATH   Validate a single workflow YAML file. When omitted,
                    both auto-format.yml and version-increment.yml under
                    .github/workflows/ are checked.
  -h, --help        Show this message.

Exits 0 when every target workflow pushes with a short-lived repo-scoped
installation token (ACTIONS_PUSH / GITHUB_TOKEN fallback). Exits non-zero
otherwise.
EOF
}

WORKFLOW=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --workflow)
      if [[ $# -lt 2 ]]; then
        echo "Missing value for --workflow" >&2
        usage >&2
        exit 2
      fi
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

WORKFLOWS=()
if [[ -n "$WORKFLOW" ]]; then
  WORKFLOWS=("$WORKFLOW")
else
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  WORKFLOWS=(
    "$SCRIPT_DIR/../.github/workflows/auto-format.yml"
    "$SCRIPT_DIR/../.github/workflows/version-increment.yml"
  )
fi

EXIT_CODE=0

validate_workflow() {
  local wf="$1"

  if [[ ! -f "$wf" ]]; then
    echo "Workflow file not found: $wf" >&2
    EXIT_CODE=2
    return
  fi

  # Emit one report line per finding:
  #   RESULT\t<ok|fail>\t<line>\t<message>
  # An indentation-aware scanner walks the YAML rather than pulling in a YAML
  # parser — the same approach as check-push-step-hardening.sh.
  local report
  report="$(
    python3 - "$wf" <<'PY'
import re
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as fh:
    lines = fh.read().splitlines()


def indent_of(line):
    return len(line) - len(line.lstrip(" "))


STEP_START = re.compile(r"^\s*-\s")
MINT_USES = re.compile(r"^\s*(?:-\s*)?uses:\s*actions/create-github-app-token@(\S+)")
SHA_REF = re.compile(r"^[0-9a-f]{40}$")
ID_LINE = re.compile(r"^\s*id:\s*(\S+)")
OWNER_INPUT = re.compile(r"^\s*owner:\s*\S")
REPOS_INPUT = re.compile(r"^\s*repositories:\s*(\S.*?)\s*$")
CONTENTS_WRITE = re.compile(r"^\s*permission-contents:\s*write\s*$")
GH_PAT_LINE = re.compile(r"^\s*GH_PAT:\s*(\S.*?)\s*$")

results = []


def report(state, lineno, message):
    results.append(f"RESULT\t{state}\t{lineno}\t{message}")


def step_bounds(index):
    """Return (start, end) line indices of the step containing `index`."""
    start = index
    while start >= 0 and not STEP_START.match(lines[start]):
        start -= 1
    if start < 0:
        return None
    step_indent = indent_of(lines[start])
    end = start + 1
    while end < len(lines):
        cur = lines[end]
        if cur.strip() and indent_of(cur) <= step_indent:
            break
        end += 1
    return start, end


mint_id = None
mint_line = 0
for idx, line in enumerate(lines):
    if line.lstrip().startswith("#"):
        continue
    match = MINT_USES.match(line)
    if not match:
        continue
    mint_line = idx + 1
    ref = match.group(1)
    if SHA_REF.match(ref):
        report("ok", mint_line, "mints the push token with SHA-pinned actions/create-github-app-token")
    else:
        report(
            "fail",
            mint_line,
            "actions/create-github-app-token must be SHA-pinned to a 40-char "
            f"commit (supply-chain policy, Issue #100) — found @{ref}",
        )

    bounds = step_bounds(idx)
    if bounds is None:
        report("fail", mint_line, "the create-github-app-token reference is not inside a step")
        break
    start, end = bounds
    body = lines[start:end]

    for raw in body:
        id_match = ID_LINE.match(raw)
        if id_match:
            mint_id = id_match.group(1)
            break
    if mint_id is None:
        report(
            "fail",
            mint_line,
            "the mint step needs an 'id:' so the push step can read its token output",
        )

    if any(CONTENTS_WRITE.match(raw) for raw in body):
        report("ok", mint_line, "narrows the minted token to 'permission-contents: write'")
    else:
        report(
            "fail",
            mint_line,
            "the mint step must request 'permission-contents: write' so the token "
            "carries only the permission the push needs",
        )

    repos = [REPOS_INPUT.match(raw) for raw in body]
    repos = [m.group(1) for m in repos if m]
    if not repos:
        report(
            "fail",
            mint_line,
            "the mint step must scope the token with 'repositories:' — an "
            "unscoped installation token reaches every repository the App is "
            "installed on",
        )
    elif len(repos) > 1 or "," in repos[0]:
        report(
            "fail",
            mint_line,
            f"'repositories:' must name this repository alone — found {repos[0]}",
        )
    else:
        report("ok", mint_line, f"scopes the minted token to a single repository ({repos[0]})")

    owner = [raw.strip() for raw in body if OWNER_INPUT.match(raw)]
    if owner:
        report(
            "fail",
            mint_line,
            "the mint step must not set 'owner:' — it widens the token beyond "
            f"this repository: {owner[0]}",
        )
    else:
        report("ok", mint_line, "sets no 'owner:' input, so the token stays repo-scoped")
    break

if mint_line == 0:
    report(
        "fail",
        0,
        "no step mints a short-lived repo-scoped push token with "
        "actions/create-github-app-token (Issue #498) — the long-lived "
        "organisation-level ACTIONS_PUSH PAT must not be the only credential",
    )

pat_bindings = [(idx + 1, m.group(1)) for idx, m in
                ((i, GH_PAT_LINE.match(line)) for i, line in enumerate(lines)) if m]

if not pat_bindings:
    report(
        "fail",
        0,
        "no step binds GH_PAT — bot pushes must authenticate with the minted "
        "app token, falling back to secrets.ACTIONS_PUSH || secrets.GITHUB_TOKEN "
        "so the resulting checks are not gated behind Approve and run (Issue #435)",
    )

FALLBACK = r"secrets\.ACTIONS_PUSH\s*\|\|\s*secrets\.GITHUB_TOKEN"
for lineno, value in pat_bindings:
    token_ref = None
    if mint_id is not None:
        token_ref = re.compile(r"steps\." + re.escape(mint_id) + r"\.outputs\.token")
    if token_ref is None or not token_ref.search(value):
        report(
            "fail",
            lineno,
            "GH_PAT must prefer the minted token "
            f"(steps.{mint_id or '<mint-step-id>'}.outputs.token) over the "
            f"organisation-level PAT — found {value}",
        )
        continue
    if not re.search(FALLBACK, value):
        report(
            "fail",
            lineno,
            "GH_PAT must fall back to 'secrets.ACTIONS_PUSH || "
            "secrets.GITHUB_TOKEN' so pushes keep clearing the Approve-and-run "
            f"gate before the App exists (Issue #435) — found {value}",
        )
        continue
    report("ok", lineno, "GH_PAT prefers the minted repo-scoped token, ACTIONS_PUSH fallback")

print("\n".join(results))
PY
  )"

  while IFS=$'\t' read -r tag state lineno message; do
    [[ "$tag" == "RESULT" ]] || continue
    if [[ "$state" == "ok" ]]; then
      echo "OK   $wf: line $lineno $message"
    else
      echo "FAIL $wf: line $lineno $message" >&2
      EXIT_CODE=1
    fi
  done <<<"$report"
}

for wf in "${WORKFLOWS[@]}"; do
  validate_workflow "$wf"
done

exit "$EXIT_CODE"
