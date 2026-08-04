#!/usr/bin/env bash
# Validate that PAT-bearing push steps cannot be poisoned by earlier
# PR-head code in the same job (Issue #497).
#
# Background: `auto-format.yml` and `version-increment.yml` execute scripts
# checked out from the **PR head branch** before a step that holds the
# org-level ACTIONS_PUSH PAT in its environment. Inside a single job, the
# earlier (attacker-editable) step can poison the later one — append a PATH
# override to $GITHUB_ENV so `git` resolves to a planted binary, or write a
# `.git/hooks/pre-commit` that runs with $GH_PAT in scope. Either exfiltrates
# an organisation-level credential.
#
# Rules enforced on every step whose `env:` binds GH_PAT to ACTIONS_PUSH:
#   1. The run block pins git to an absolute path (`GIT=/usr/bin/git`), so a
#      $GITHUB_ENV PATH override cannot redirect the invocation.
#   2. No bare `git` command word remains in the block — every invocation goes
#      through "$GIT".
#   3. Every "$GIT" invocation passes `-c core.hooksPath=/dev/null`, so planted
#      repository hooks never execute with the PAT in scope.
#   4. The block executes no repository script (`./scripts/...`), which would
#      hand the PAT straight to PR-head code.
#   5. `base64` is likewise pinned to an absolute path (`BASE64=/usr/bin/base64`)
#      and never invoked bare: it is piped the PAT on stdin when the auth
#      header is built, so a planted `base64` reads the credential directly.
#
# See GitHub's security-hardening guidance on limiting the scope of
# credentials available to workflow-executed code.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/check-harness.sh
source "$SCRIPT_DIR/lib/check-harness.sh"

usage() {
  cat <<'EOF'
Usage: check-push-step-hardening.sh [--workflow PATH]

Options:
  --workflow PATH   Validate a single workflow YAML file. When omitted, both
                    auto-format.yml and version-increment.yml under
                    .github/workflows/ are checked.
  -h, --help        Show this message.

Exits 0 when every PAT-bearing push step is hardened against in-job poisoning
by earlier PR-head steps. Exits non-zero otherwise.
EOF
}

parse_check_args --workflow "" "$@"
WORKFLOW="$CHECK_TARGET"

WORKFLOWS=()
if [[ -n "$WORKFLOW" ]]; then
  WORKFLOWS=("$WORKFLOW")
else
  WORKFLOWS=(
    "$(check_repo_path ".github/workflows/auto-format.yml")"
    "$(check_repo_path ".github/workflows/version-increment.yml")"
  )
fi

validate_workflow() {
  local wf="$1"

  if [[ ! -f "$wf" ]]; then
    echo "Workflow file not found: $wf" >&2
    EXIT_CODE=2
    return
  fi

  # Emit one report line per finding:
  #   RESULT\t<ok|fail>\t<step line>\t<message>
  # An indentation-aware scanner walks the YAML rather than pulling in a YAML
  # parser — the same approach as check-persist-credentials.sh.
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


PAT_ENV = re.compile(r"GH_PAT:.*secrets\.ACTIONS_PUSH")
STEP_START = re.compile(r"^\s*-\s")
RUN_BLOCK = re.compile(r"^\s*run:\s*\|")
ABS_GIT = re.compile(r"""^\s*GIT=(["']?)/usr/bin/git\1\s*$""")
BARE_GIT = re.compile(r"""(?:^|[;&|(]\s*|\$\(\s*|`\s*)git\s""")
SAFE_GIT = re.compile(r"""["']?\$\{?GIT\}?["']?\s""")
HOOKS_OFF = re.compile(r"-c\s+core\.hooksPath=/dev/null")
REPO_SCRIPT = re.compile(r"\./scripts/")
# Derived from the git patterns rather than re-spelt: identical rules, and it
# keeps this heredoc free of extra quote/paren/backtick tokens (bash 3.2 scans
# the body of a $(...) command substitution for those).
ABS_BASE64 = re.compile(ABS_GIT.pattern.replace("GIT", "BASE64").replace("bin/git", "bin/base64"))
BARE_BASE64 = re.compile(BARE_GIT.pattern.replace("git", "base64"))
USES_BASE64 = re.compile(r"\bbase64\b")

results = []


def report(state, lineno, message):
    results.append(f"RESULT\t{state}\t{lineno}\t{message}")


def step_bounds(env_index):
    """Return (start, end) line indices of the step containing env_index."""
    start = env_index
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


def run_block(start, end):
    """Return the literal run: block body of a step, or None."""
    for i in range(start, end):
        if RUN_BLOCK.match(lines[i]):
            body_indent = indent_of(lines[i])
            body = []
            for j in range(i + 1, end):
                cur = lines[j]
                if cur.strip() and indent_of(cur) <= body_indent:
                    break
                body.append(cur)
            return body
    return None


def logical_lines(body):
    """Join backslash continuations so a wrapped command is checked as one."""
    joined, buf = [], ""
    for raw in body:
        stripped = raw.strip()
        if stripped.endswith("\\"):
            buf += stripped[:-1] + " "
            continue
        joined.append(buf + stripped)
        buf = ""
    if buf:
        joined.append(buf)
    return [line for line in joined if line and not line.startswith("#")]


pat_steps = 0
for idx, line in enumerate(lines):
    if not PAT_ENV.search(line):
        continue
    pat_steps += 1
    bounds = step_bounds(idx)
    if bounds is None:
        report("fail", idx + 1, "GH_PAT binding is not inside a step")
        continue
    start, end = bounds
    lineno = start + 1
    body = run_block(start, end)
    if body is None:
        report("fail", lineno, "PAT-bearing step has no literal 'run: |' block to validate")
        continue

    cmds = logical_lines(body)

    if any(ABS_GIT.match(raw) for raw in body):
        report("ok", lineno, "pins git to an absolute path (GIT=/usr/bin/git)")
    else:
        report(
            "fail",
            lineno,
            "PAT-bearing step must pin git to an absolute path "
            "(GIT=/usr/bin/git) — a $GITHUB_ENV PATH override set by earlier "
            "PR-head code would otherwise redirect it",
        )

    bare = [c for c in cmds if BARE_GIT.search(c)]
    if bare:
        report("fail", lineno, f"bare 'git' invocation must use \"$GIT\": {bare[0]}")
    else:
        report("ok", lineno, "no bare 'git' command word — all invocations use \"$GIT\"")

    git_cmds = [c for c in cmds if SAFE_GIT.search(c)]
    if not git_cmds:
        report("fail", lineno, 'no "$GIT" invocation found in the PAT-bearing step')
    else:
        unhooked = [c for c in git_cmds if not HOOKS_OFF.search(c)]
        if unhooked:
            report(
                "fail",
                lineno,
                "every \"$GIT\" invocation must pass -c core.hooksPath=/dev/null "
                f"so planted repository hooks cannot read $GH_PAT: {unhooked[0]}",
            )
        else:
            report("ok", lineno, "every \"$GIT\" invocation disables repository hooks")

    # base64 is only reached when the step builds the auth header itself; a
    # step that never mentions it has nothing to pin.
    if any(USES_BASE64.search(c) for c in cmds):
        bare_b64 = [c for c in cmds if BARE_BASE64.search(c)]
        if not any(ABS_BASE64.match(raw) for raw in body):
            report(
                "fail",
                lineno,
                "PAT-bearing step must pin base64 to an absolute path "
                "(BASE64=/usr/bin/base64) — it is piped $GH_PAT on stdin, so a "
                "planted base64 reached through an overridden PATH reads the PAT",
            )
        elif bare_b64:
            report(
                "fail",
                lineno,
                f"bare 'base64' invocation must use \"$BASE64\": {bare_b64[0]}",
            )
        else:
            report("ok", lineno, "pins base64 to an absolute path (BASE64=/usr/bin/base64)")

    scripted = [c for c in cmds if REPO_SCRIPT.search(c)]
    if scripted:
        report(
            "fail",
            lineno,
            "PAT-bearing step must not execute repository scripts — PR-head "
            f"code would run with $GH_PAT in its environment: {scripted[0]}",
        )
    else:
        report("ok", lineno, "executes no repository script with the PAT in scope")

if pat_steps == 0:
    report("fail", 0, "no step binds GH_PAT to secrets.ACTIONS_PUSH — nothing to validate")

print("\n".join(results))
PY
  )"

  while IFS=$'\t' read -r tag state lineno message; do
    [[ "$tag" == "RESULT" ]] || continue
    if [[ "$state" == "ok" ]]; then
      echo "OK   $wf: step at line $lineno $message"
    else
      echo "FAIL $wf: step at line $lineno $message" >&2
      EXIT_CODE=1
    fi
  done <<<"$report"
}

for wf in "${WORKFLOWS[@]}"; do
  validate_workflow "$wf"
done

exit "$EXIT_CODE"
