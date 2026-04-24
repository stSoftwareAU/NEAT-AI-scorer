#!/usr/bin/env bash
# Validate GitHub Actions versions used across workflows for Node 24
# compatibility (Issue #24).
#
# Why this exists:
#   * GitHub has begun deprecating the Node 20 runtime for custom actions. Any
#     action pinned to a major version that still runs on Node 20 emits a
#     deprecation warning on every run and will eventually stop working on
#     hosted runners.
#   * A hand-rolled check is lighter than dependabot-for-actions and keeps
#     policy co-located with the rest of our CI helpers. It also lets us
#     record explicit, auditable *exceptions* (upstream actions that have no
#     Node 24 release yet).
#
# Policy encoded below (in `lookup_policy`):
#   * `required:<N>` — action MUST be pinned to at least major version N. If
#     a workflow references an older major (or a non-numeric ref like
#     `master`/`main`), the script fails.
#   * `node20:<N>` — action's latest stable major still uses Node 20. The
#     workflow must stay on exactly major N until upstream ships a Node 24
#     release. Each exception is documented inline.
#   * `no-node` — composite/shell action that does not ship a Node runtime.
#     Allowed to use non-semver refs (e.g. `@stable`).
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

Exits 0 when every `uses:` reference satisfies the Node 24 compatibility
policy. Exits non-zero with a descriptive message otherwise.
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

if [[ -z "$WORKFLOWS_DIR" ]]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  WORKFLOWS_DIR="$SCRIPT_DIR/../.github/workflows"
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
    peter-evans/create-pull-request)  echo "required:8" ;;
    ludeeus/action-shellcheck)        echo "required:2" ;;
    # actions/dependency-review-action: action.yml declares `using: node20`
    # at v4.9.x (2026-04). No v5 release exists yet. Revisit when upstream
    # ships a Node 24 runtime.
    actions/dependency-review-action) echo "node20:4" ;;
    # rustsec/audit-check: v2.0.0 (2025) still uses Node 20. No v3 exists.
    # Revisit when upstream publishes a Node 24 release.
    rustsec/audit-check)              echo "node20:2" ;;
    # dtolnay/rust-toolchain is a composite/shell action. No Node runtime,
    # so the Node 24 deprecation does not apply.
    dtolnay/rust-toolchain)           echo "no-node" ;;
    *) echo "" ;;
  esac
}

# Scan a single workflow file and emit tab-separated records:
#   <file>\t<line>\t<action>\t<ref>
scan_workflow() {
  local file="$1"
  python3 - "$file" <<'PY'
import re
import sys

path = sys.argv[1]
pattern = re.compile(r"^\s*(?:-\s*)?uses:\s*([^@\s]+)@([^\s#]+)")
with open(path, "r", encoding="utf-8") as fh:
    for lineno, raw in enumerate(fh, start=1):
        # Skip comment lines so policy examples in comments do not trigger.
        stripped = raw.lstrip()
        if stripped.startswith("#"):
            continue
        match = pattern.match(raw)
        if not match:
            continue
        action, ref = match.group(1), match.group(2)
        # Reusable workflow calls like `./.github/workflows/security.yml`
        # do not carry a version; skip them.
        if action.startswith("./"):
            continue
        print(f"{path}\t{lineno}\t{action}\t{ref}")
PY
}

# Parse the major version from a ref like "v5", "v5.0.1", "5", or "master".
# Sets MAJOR to the parsed integer, or empty when the ref has no numeric
# major component (branch ref).
parse_major() {
  local ref="$1"
  MAJOR=""
  local trimmed="${ref#v}"
  local digits="${trimmed%%[^0-9]*}"
  if [[ -n "$digits" ]]; then
    MAJOR="$digits"
  fi
}

EXIT_CODE=0
FOUND_ANY=0

while IFS= read -r workflow; do
  [[ -f "$workflow" ]] || continue
  FOUND_ANY=1

  findings="$(scan_workflow "$workflow")"
  [[ -z "$findings" ]] && continue

  while IFS=$'\t' read -r file lineno action ref; do
    [[ -z "${action:-}" ]] && continue

    policy="$(lookup_policy "$action")"
    parse_major "$ref"
    major="$MAJOR"

    case "$policy" in
      required:*)
        want="${policy#required:}"
        if [[ -z "$major" ]]; then
          echo "FAIL $file:$lineno: $action@$ref — branch ref disallowed; pin to v$want or newer (Node 24 compat)." >&2
          EXIT_CODE=1
        elif (( major < want )); then
          echo "FAIL $file:$lineno: $action@$ref — requires @v$want or newer (Node 24 compat)." >&2
          EXIT_CODE=1
        else
          echo "OK   $file:$lineno: $action@$ref (>= v$want)"
        fi
        ;;
      node20:*)
        want="${policy#node20:}"
        if [[ -z "$major" || "$major" != "$want" ]]; then
          echo "FAIL $file:$lineno: $action@$ref — tracked Node 20 exception must stay on @v$want until upstream ships a Node 24 release." >&2
          EXIT_CODE=1
        else
          echo "OK   $file:$lineno: $action@$ref (Node 20 exception, tracked)"
        fi
        ;;
      no-node)
        echo "OK   $file:$lineno: $action@$ref (no Node runtime — policy not applicable)"
        ;;
      "")
        echo "WARN $file:$lineno: $action@$ref — not in policy tables; review and add to check-workflow-action-versions.sh."
        ;;
    esac
  done <<< "$findings"
done < <(find "$WORKFLOWS_DIR" -maxdepth 1 \( -name "*.yml" -o -name "*.yaml" \) -type f | sort)

if [[ "$FOUND_ANY" -eq 0 ]]; then
  echo "No workflow files found in $WORKFLOWS_DIR" >&2
  exit 2
fi

exit "$EXIT_CODE"
