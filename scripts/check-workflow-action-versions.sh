#!/usr/bin/env bash
# Validate GitHub Actions across workflows for SHA pinning (Issue #100) and
# Node 24 compatibility (Issue #24).
#
# Why this exists:
#   * Supply-chain defence (Issue #100). A `uses: foo/bar@v1` line resolves
#     to whatever commit the tag currently points at; a compromised maintainer
#     can re-push the tag to a malicious SHA and silently re-execute under
#     this repo's workflow privileges (`contents: write`, `GITHUB_TOKEN`).
#     Pinning every `uses:` to a 40-character commit SHA freezes the action
#     to a specific reviewed commit. Bumps then become explicit, reviewable
#     changes in PRs.
#   * Node 24 deprecation (Issue #24). Any action whose latest major still
#     runs on Node 20 emits a deprecation warning on every CI run and will
#     eventually stop working on hosted runners. We track this via the
#     trailing version comment, not the SHA itself.
#   * A hand-rolled check is lighter than dependabot-for-actions and keeps
#     policy co-located with the rest of our CI helpers. It also lets us
#     record explicit, auditable *exceptions* (upstream actions that have no
#     Node 24 release yet).
#
# Required form: every `uses:` line MUST look like
#
#     uses: owner/repo@<40-char-hex-sha>  # <version-or-ref label>
#
# The 40-char SHA is the supply-chain pin. The trailing `# <label>` records
# the human-readable upstream version (e.g. `v5`, `v8.1.0`,
# `stable, frozen 2026-05-18`) so reviewers can spot drift at a glance and
# bumps stay auditable. The label is also what the Node 24 policy below
# validates against.
#
# Policy encoded below (in `lookup_policy`), keyed on the label:
#   * `required:<N>` — action MUST track at least major version N. The label
#     must start with `v<M>` where M >= N (or be `vM.x.y` etc.).
#   * `node20:<N>` — action's latest stable major still uses Node 20. The
#     label must be exactly `v<N>` (or `v<N>.x.y`) until upstream ships a
#     Node 24 release. Each exception is documented inline.
#   * `no-node` — composite/shell action that does not ship a Node runtime.
#     Any descriptive label is permitted (e.g. `stable, frozen YYYY-MM-DD`).
#   * (no match) — unknown action, warn but do not fail. New actions should
#     be added to the policy explicitly so they get audited.
#
# Implementation note: bash 3.2 (macOS default) has no associative arrays,
# so policy lookup is a `case` statement.
#
# Designed for reuse from BATS tests and `quality.sh`.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-workflow-action-versions.sh [--workflows DIR]

Options:
  --workflows DIR   Directory containing workflow YAML files (default:
                    .github/workflows relative to the repo root).
  -h, --help        Show this message.

Exits 0 when every `uses:` reference is SHA-pinned and satisfies the Node 24
compatibility policy. Exits non-zero with a descriptive message otherwise.
EOF
}

WORKFLOWS_DIR=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --workflows)
      WORKFLOWS_DIR="$2"
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

# In default mode also scan local composite actions: the SHA-pinned
# `actions/checkout` for NEAT-AI-core now lives in
# `.github/actions/setup-neat-core/action.yml` (Issue #401), so the pinning
# policy must keep covering it. An explicit --workflows override scans only that
# directory (keeps test fixtures isolated).
ACTIONS_DIR=""
if [[ -z "$WORKFLOWS_DIR" ]]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  WORKFLOWS_DIR="$SCRIPT_DIR/../.github/workflows"
  ACTIONS_DIR="$SCRIPT_DIR/../.github/actions"
fi

if [[ ! -d "$WORKFLOWS_DIR" ]]; then
  echo "Workflows directory not found: $WORKFLOWS_DIR" >&2
  exit 2
fi

# Look up the policy for a given action. Prints one of:
#   required:<N>    — action must be pinned at @vN or newer (Node 24 bump)
#   node20:<N>      — tracked Node 20 exception; must stay on exactly @vN
#   no-node         — composite/shell action; no Node runtime
#   (empty)         — unknown action; caller emits a WARN
lookup_policy() {
  local action="$1"
  case "$action" in
    actions/checkout)                 echo "required:5" ;;
    actions/cache)                    echo "required:5" ;;
    # actions/upload-artifact: v4 and v5 still ship a Node 20 runtime; v6.0.0
    # (2025) is the first Node 24 release, and v7.0.1 is the current line.
    # Pinning the floor at v6 keeps the SBOM workflow (Issue #172) clear of
    # the Node 20 deprecation.
    actions/upload-artifact)          echo "required:6" ;;
    # actions/setup-node v4 and v5 still ship a Node 20 runtime; v6.0.0
    # (2025-10) is the first Node 24 release. Pinning the floor at v6
    # prevents the deprecation regression that triggered Issue #137.
    actions/setup-node)               echo "required:6" ;;
    peter-evans/create-pull-request)  echo "required:8" ;;
    ludeeus/action-shellcheck)        echo "required:2" ;;
    # actions/dependency-review-action: v5.0.0 (2026-05-08) ships on Node 24.
    # The previous v4.9.x line was a tracked Node 20 exception — Issue #136
    # cleared it once upstream published the Node 24 release.
    actions/dependency-review-action) echo "required:5" ;;
    # rustsec/audit-check: the v2.0.0 tag (2024-09-23) still declares
    # `using: node20`, but master HEAD post upstream #48 (commit
    # 858dc40, 2026-03-20) is `using: node24`. No v2.1 / v3 tag exists yet,
    # so we SHA-pin to the master commit and label it `# v2 (master HEAD,
    # Node 24, post upstream #48)`. Issue #136 cleared the Node 20 exception
    # when we adopted that SHA.
    rustsec/audit-check)              echo "required:2" ;;
    # taiki-e/install-action installs prebuilt cargo tool binaries (Issue
    # #208), replacing source compiles. The v2 line ships a Node 24 runtime.
    taiki-e/install-action)           echo "required:2" ;;
    # dtolnay/rust-toolchain is a composite/shell action. No Node runtime,
    # so the Node 24 deprecation does not apply.
    dtolnay/rust-toolchain)           echo "no-node" ;;
    *) echo "" ;;
  esac
}

# Scan a single workflow file and emit tab-separated records:
#   <file>\t<line>\t<action>\t<ref>\t<comment>
# `<comment>` is the trailing `# ...` text (empty when absent). The
# comment is where reviewers — and this script — read the human-readable
# version label that backs the SHA pin.
scan_workflow() {
  local file="$1"
  python3 - "$file" <<'PY'
import re
import sys

path = sys.argv[1]
pattern = re.compile(
    r"^\s*(?:-\s*)?uses:\s*([^@\s]+)@(\S+?)(?:\s+#\s*(.*?))?\s*$"
)
with open(path, "r", encoding="utf-8") as fh:
    for lineno, raw in enumerate(fh, start=1):
        # Skip comment lines so policy examples in comments do not trigger.
        stripped = raw.lstrip()
        if stripped.startswith("#"):
            continue
        match = pattern.match(raw.rstrip("\n"))
        if not match:
            continue
        action, ref, comment = match.group(1), match.group(2), (match.group(3) or "")
        # Reusable workflow calls like `./.github/workflows/security.yml`
        # do not carry a version; skip them.
        if action.startswith("./"):
            continue
        print(f"{path}\t{lineno}\t{action}\t{ref}\t{comment}")
PY
}

# Extract a vN-style major from a label like "v5", "v5.0.1", or
# "v8.x.y". Returns empty when the label does not start with `v<digits>`.
parse_label_major() {
  local label="$1"
  LABEL_MAJOR=""
  if [[ "$label" =~ ^v([0-9]+) ]]; then
    LABEL_MAJOR="${BASH_REMATCH[1]}"
  fi
}

SHA_RE='^[0-9a-f]{40}$'

EXIT_CODE=0
FOUND_ANY=0

while IFS= read -r workflow; do
  [[ -f "$workflow" ]] || continue
  FOUND_ANY=1

  findings="$(scan_workflow "$workflow")"
  [[ -z "$findings" ]] && continue

  while IFS=$'\t' read -r file lineno action ref comment; do
    [[ -z "${action:-}" ]] && continue

    policy="$(lookup_policy "$action")"

    # Supply-chain gate: every `uses:` must be SHA-pinned (Issue #100).
    if ! [[ "$ref" =~ $SHA_RE ]]; then
      echo "FAIL $file:$lineno: $action@$ref — not SHA-pinned; pin to a 40-character commit SHA with a trailing '# <version>' comment (Issue #100)." >&2
      EXIT_CODE=1
      continue
    fi

    # SHA-pinned: validate the trailing comment against the Node 24 policy.
    if [[ -z "$comment" ]]; then
      echo "FAIL $file:$lineno: $action@$ref — SHA-pinned but missing trailing '# <version>' comment; required for reviewability (Issue #100)." >&2
      EXIT_CODE=1
      continue
    fi

    parse_label_major "$comment"
    major="$LABEL_MAJOR"

    case "$policy" in
      required:*)
        want="${policy#required:}"
        if [[ -z "$major" ]]; then
          echo "FAIL $file:$lineno: $action@$ref ($comment) — version comment must start with v$want or newer (Node 24 compat)." >&2
          EXIT_CODE=1
        elif (( major < want )); then
          echo "FAIL $file:$lineno: $action@$ref ($comment) — requires v$want or newer (Node 24 compat)." >&2
          EXIT_CODE=1
        else
          echo "OK   $file:$lineno: $action@$ref ($comment) (>= v$want, SHA-pinned)"
        fi
        ;;
      node20:*)
        want="${policy#node20:}"
        if [[ -z "$major" || "$major" != "$want" ]]; then
          echo "FAIL $file:$lineno: $action@$ref ($comment) — tracked Node 20 exception must stay on v$want until upstream ships a Node 24 release." >&2
          EXIT_CODE=1
        else
          echo "OK   $file:$lineno: $action@$ref ($comment) (Node 20 exception, tracked, SHA-pinned)"
        fi
        ;;
      no-node)
        echo "OK   $file:$lineno: $action@$ref ($comment) (no Node runtime — SHA-pinned)"
        ;;
      "")
        echo "WARN $file:$lineno: $action@$ref ($comment) — not in policy tables; review and add to check-workflow-action-versions.sh."
        ;;
    esac
  done <<< "$findings"
done < <(
  {
    find "$WORKFLOWS_DIR" -maxdepth 1 \( -name "*.yml" -o -name "*.yaml" \) -type f
    if [[ -n "$ACTIONS_DIR" && -d "$ACTIONS_DIR" ]]; then
      find "$ACTIONS_DIR" \( -name "action.yml" -o -name "action.yaml" \) -type f
    fi
  } | sort
)

if [[ "$FOUND_ANY" -eq 0 ]]; then
  echo "No workflow files found in $WORKFLOWS_DIR" >&2
  exit 2
fi

exit "$EXIT_CODE"
