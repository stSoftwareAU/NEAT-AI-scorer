#!/usr/bin/env bash
# Validate the standalone Cargo Security Audit workflow (Issues #64, #603).
#
# The cargo-audit workflow must:
#   1. Trigger on a cron schedule so freshly published advisories surface even
#      on quiet branches (the lockfile does not change but the advisory
#      database does). This is the standalone workflow's non-redundant value.
#   2. Declare an explicit `permissions:` block (least privilege).
#   3. Pin `actions/checkout` to a numeric major version or a 40-char SHA
#      (Node 24 policy).
#   4. Install a Rust toolchain via `dtolnay/rust-toolchain` so cargo-audit
#      can be installed and run.
#   5. Invoke cargo-audit — either directly (`cargo audit` after a
#      `cargo install cargo-audit` step) or via the official RustSec
#      action (`rustsec/audit-check@vN`).
#   6. Cover milestone/<slug> branches in the pull_request filter, *when it has
#      one*, so the gate runs on milestone sub-issue PRs and not only on
#      top-level base branches (Issue #391). A bare "*" glob does not match
#      refs containing a slash.
#   7. Leave exactly ONE PR-time `cargo audit` across `.github/workflows`
#      (Issue #603). A `pull_request` trigger here is optional: what matters is
#      that some workflow audits `Cargo.lock` on a pull request, and that no
#      second one repeats the identical verdict for double the runner minutes.
#
# Rule 7 mirrors the ShellCheck dedup invariant (Issue #157,
# `check-shellcheck-dedup.sh`): a logical check belongs in exactly one home.
# "PR-time" includes an audit reached indirectly — `ci.yml` fires on
# `pull_request` and calls the reusable `security.yml`, so `security.yml`'s
# `rustsec/audit-check` step counts.
#
# A duplicate is reported with `warn`, not `fail`: removing it means editing
# `.github/workflows/`, which the automation worker has no `workflow` OAuth
# scope to push (CONTRIBUTING "Human escalation"). The WARN keeps the
# deficiency loud on every gate run until a maintainer lands that edit —
# missing PR-time coverage altogether is a hard FAIL, because that is a real
# security regression rather than a redundancy.
#
# The script takes optional `--workflow PATH` and `--workflows DIR` arguments so
# BATS tests can exercise it against fixtures. With no argument it validates
# `.github/workflows/cargo-audit.yml` against the workflows directory beside it.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Shared CLI harness (Issue #512) — the ok/fail/warn reporting primitives and
# the not-found guards.
# shellcheck source=scripts/lib/check-harness.sh
source "$SCRIPT_DIR/lib/check-harness.sh"

# Shared workflow-validation helpers (Issues #511, #514) — the `actions/checkout`
# pin rule and the least-privilege `permissions:` rule each live in one place
# instead of inline copies that drift apart.
WORKFLOW_CHECKS_LIB="$SCRIPT_DIR/lib/workflow-checks.sh"
check_require_file "$WORKFLOW_CHECKS_LIB" "Shared helper"
# shellcheck source=lib/workflow-checks.sh disable=SC1091
source "$WORKFLOW_CHECKS_LIB"

usage() {
  cat <<'EOF'
Usage: check-cargo-audit-workflow.sh [--workflow PATH] [--workflows DIR]

Options:
  --workflow PATH   Path to the cargo-audit workflow YAML file (default:
                    .github/workflows/cargo-audit.yml relative to the repo root).
  --workflows DIR   Directory scanned for other PR-time cargo audits (default:
                    the directory containing the workflow above).
  -h, --help        Show this message.

Exits 0 when the workflow satisfies every rule listed in the script header.
Exits non-zero with a descriptive message otherwise.
EOF
}

WORKFLOW=""
WORKFLOWS_DIR=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --workflow)
      check_flag_value "$1" $#
      WORKFLOW="$2"
      shift 2
      ;;
    --workflows)
      check_flag_value "$1" $#
      WORKFLOWS_DIR="$2"
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

if [[ -z "$WORKFLOW" ]]; then
  WORKFLOW="$(check_repo_path ".github/workflows/cargo-audit.yml")"
fi
check_require_file "$WORKFLOW"

if [[ -z "$WORKFLOWS_DIR" ]]; then
  WORKFLOWS_DIR="$(dirname "$WORKFLOW")"
fi
check_require_dir "$WORKFLOWS_DIR"

check_subject "$WORKFLOW"

# triggers_on_pull_request <workflow> — true when the workflow's `on:` map has a
# `pull_request:` key (block form) or names it in the inline list form. The
# trailing colon keeps `pull_request_target:` out.
triggers_on_pull_request() {
  grep -qE '^[[:space:]]+pull_request:' "$1" \
    || grep -qE '^on:[[:space:]]*\[?.*pull_request([],[:space:]]|$)' "$1"
}

# invokes_cargo_audit <workflow> — true when the workflow actually runs an
# audit: the RustSec action on a `uses:` line, or `cargo audit` inside a `run:`
# step. Both patterns anchor to the step keyword so prose in a comment — of
# which `security.yml` has several — never counts as an invocation.
invokes_cargo_audit() {
  grep -qE '^[[:space:]]*(-[[:space:]]*)?uses:[[:space:]]*rustsec/audit-check@' "$1" \
    || grep -qE '^[[:space:]]*(-[[:space:]]*)?run:[[:space:]]*.*\bcargo[[:space:]]+audit\b' "$1"
}

# pr_time_audit_workflows <dir> — print every workflow in DIR that audits
# `Cargo.lock` on a pull request, one path per line. Reachability starts at the
# workflows with a `pull_request` trigger and follows local `uses:
# ./.github/workflows/<file>` calls to a fixed point, so an audit inside a
# reusable workflow is attributed to the PR event that reaches it.
pr_time_audit_workflows() {
  local dir="$1"
  local -a frontier=() reachable=()
  local workflow callee name

  while IFS= read -r workflow; do
    [[ -f "$workflow" ]] || continue
    if triggers_on_pull_request "$workflow"; then
      frontier+=("$workflow")
    fi
  done < <(find "$dir" -maxdepth 1 \( -name "*.yml" -o -name "*.yaml" \) -type f | sort)

  while [[ ${#frontier[@]} -gt 0 ]]; do
    workflow="${frontier[0]}"
    frontier=("${frontier[@]:1}")
    # Skip anything already seen so a call cycle cannot loop forever.
    if [[ " ${reachable[*]+"${reachable[*]}"} " == *" $workflow "* ]]; then
      continue
    fi
    reachable+=("$workflow")
    while IFS= read -r name; do
      callee="$dir/$name"
      [[ -f "$callee" ]] && frontier+=("$callee")
    done < <(sed -nE 's|^[[:space:]]*(-[[:space:]]*)?uses:[[:space:]]*\./\.github/workflows/([^[:space:]]+).*|\2|p' "$workflow")
  done

  for workflow in ${reachable[@]+"${reachable[@]}"}; do
    if invokes_cargo_audit "$workflow"; then
      echo "$workflow"
    fi
  done
}

# 1. Schedule trigger present (cron) — covers advisory-DB updates.
if grep -qE '^[[:space:]]+schedule:' "$WORKFLOW" \
  && grep -qE '^[[:space:]]+- cron:' "$WORKFLOW"; then
  ok "triggers on schedule (cron)"
else
  fail "no schedule (cron) trigger — weekly audit run required"
fi

# 2. Explicit permissions block (least privilege), via the shared rule in
#    scripts/lib/workflow-checks.sh (Issue #514).
require_readonly_permissions "$WORKFLOW"

# 3. actions/checkout pinned to a numeric major (vN) or a 40-char SHA, via the
# shared rule in scripts/lib/workflow-checks.sh (Issue #511). Branch refs
# (e.g. `@main`) disallowed. Issue #136 widened the regex from `v?[0-9]+` so
# SHAs starting with hex letters a–f are accepted too; keeping that rule in one
# place is why Issue #511 extracted the helper.
require_pinned_checkout "$WORKFLOW"

# 4. Rust toolchain provisioned via dtolnay/rust-toolchain.
if grep -qE 'uses:[[:space:]]*dtolnay/rust-toolchain@' "$WORKFLOW"; then
  ok "dtolnay/rust-toolchain present"
else
  fail "dtolnay/rust-toolchain missing — Rust toolchain not provisioned"
fi

# 5. cargo-audit invoked — either via the rustsec action OR a direct
#    `cargo audit` run (after `cargo install cargo-audit`).
if grep -qE 'uses:[[:space:]]*rustsec/audit-check@' "$WORKFLOW"; then
  ok "cargo audit invoked via rustsec/audit-check action"
elif grep -qE '^[[:space:]]+(- )?run:[[:space:]]*cargo audit' "$WORKFLOW" \
  || grep -qE 'cargo[[:space:]]+audit([[:space:]]|$)' "$WORKFLOW"; then
  ok "cargo audit invoked directly"
else
  fail "cargo audit is not invoked — neither rustsec/audit-check nor a 'cargo audit' run step found"
fi

# 6. pull_request branch filter must include milestone branches so the gate
#    runs on milestone/<slug> sub-issue PRs. A bare "*" glob only matches
#    single-level refs, so milestone/<slug> would otherwise skip the gate and
#    merge unchecked into the milestone branch (Issue #391). The token
#    `milestone/*` also matches the wider `milestone/**` glob. The rule is
#    conditional: a workflow that does not gate PRs at all has no filter to
#    check (Issue #603).
if triggers_on_pull_request "$WORKFLOW"; then
  ok "triggers on pull_request"
  if grep -qE 'milestone/\*' "$WORKFLOW"; then
    ok "pull_request branch filter covers milestone/* branches"
  else
    fail "pull_request branch filter omits milestone/* — milestone PRs skip the gate"
  fi
else
  ok "no pull_request trigger — schedule/dispatch only, PR-time auditing lives elsewhere (Issue #603)"
fi

# 7. Exactly one PR-time cargo audit across the workflows directory (Issue #603).
mapfile -t PR_AUDITS < <(pr_time_audit_workflows "$WORKFLOWS_DIR")
case "${#PR_AUDITS[@]}" in
  1)
    ok "exactly one PR-time cargo audit across $WORKFLOWS_DIR: ${PR_AUDITS[0]}"
    ;;
  0)
    fail "no workflow runs a cargo audit on pull_request — PR-time advisory coverage is missing"
    ;;
  *)
    warn "PR-time cargo audit duplicated across ${#PR_AUDITS[@]} workflows — both read the same Cargo.lock against the same advisory database on the same pull_request event, so the second run can only repeat the first verdict at double the runner minutes (Issue #603): ${PR_AUDITS[*]}. Drop the standalone workflow's pull_request trigger (keeping its weekly cron) or the reusable workflow's audit step — the edit is under .github/workflows/ and needs a maintainer (CONTRIBUTING \"Human escalation\")"
    ;;
esac

exit "$EXIT_CODE"
