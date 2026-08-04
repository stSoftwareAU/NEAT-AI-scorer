# Shared workflow-validation helpers (Issues #511, #514).
#
# Sourced by the per-workflow validators under `scripts/`. It carries rules that
# every one of them enforces identically, so a change to the rule is a change to
# one line rather than six copies that drift apart. Issue #511 was filed after
# the `actions/checkout` pin rule had already split into three generations of
# the same regex.
#
# Contract for callers: define `ok <message>` and `fail <message>` before
# calling a helper (every validator already does — `fail` sets `EXIT_CODE=1`).
# The helpers report through those functions so output stays uniform, and abort
# with exit status 2 when the contract or an argument is wrong rather than
# silently reporting a pass.
#
# This file is sourced, not executed — it deliberately has no shebang and sets
# no shell options of its own.

# Accepted `actions/checkout` pin forms: a numeric major (`v5`) or a full
# 40-character commit SHA. The trailing `\b` is load-bearing — without it a
# 41-character ref would match on its first 40 characters. Branch refs (`main`),
# truncated SHAs and tags that merely start with a digit are rejected.
#
# `scripts/check-workflow-action-versions.sh` enforces the strictly stronger
# SHA-only rule across every workflow; this helper is the per-workflow gate that
# also catches a checkout step going missing entirely.
CHECKOUT_PIN_RE='^(v[0-9]+|[0-9a-f]{40})\b'

# require_pinned_checkout <workflow>
#
# Reports OK when the workflow has at least one `actions/checkout` step and
# every such step is pinned to an accepted form; reports FAIL otherwise.
require_pinned_checkout() {
  local workflow="${1:-}"

  if [[ -z "$workflow" ]]; then
    echo "require_pinned_checkout: a workflow path argument is required" >&2
    return 2
  fi
  if [[ ! -r "$workflow" ]]; then
    echo "require_pinned_checkout: workflow not readable: $workflow" >&2
    return 2
  fi
  if ! declare -F ok >/dev/null || ! declare -F fail >/dev/null; then
    echo "require_pinned_checkout: caller must define ok() and fail() first" >&2
    return 2
  fi

  local line ref
  local -a refs=()
  local -a unpinned=()
  while IFS= read -r line; do
    ref="${line##*actions/checkout@}"
    refs+=("$ref")
    if ! printf '%s\n' "$ref" | grep -qE "$CHECKOUT_PIN_RE"; then
      unpinned+=("$ref")
    fi
  done < <(grep -oE 'uses:[[:space:]]*actions/checkout@[^[:space:]]+' "$workflow" || true)

  if [[ "${#refs[@]}" -eq 0 ]]; then
    fail "actions/checkout step missing — workflow cannot fetch the repo"
  elif [[ "${#unpinned[@]}" -gt 0 ]]; then
    fail "actions/checkout is not pinned — branch refs disallowed: ${unpinned[*]}"
  else
    ok "actions/checkout pinned to a numeric major or 40-char SHA"
  fi
}

# require_readonly_permissions <workflow>
#
# The least-privilege rule (Issue #514): the workflow declares a bare top-level
# `permissions:` key and grants `contents: read` beneath it. A bare key is
# required so the blanket `permissions: write-all` shorthand is rejected — an
# inline `permissions: {contents: read}` one-liner is not accepted either, and
# no workflow in this repo uses one.
#
# Known looseness, deliberately kept when the seven inline copies were unified:
# the two greps are independent, so an indented `contents: read` anywhere in the
# file satisfies the second one. Tightening it to "inside the top-level block"
# is now a single edit here.
require_readonly_permissions() {
  local workflow="${1:-}"

  if [[ -z "$workflow" ]]; then
    echo "require_readonly_permissions: a workflow path argument is required" >&2
    return 2
  fi
  if [[ ! -r "$workflow" ]]; then
    echo "require_readonly_permissions: workflow not readable: $workflow" >&2
    return 2
  fi
  if ! declare -F ok >/dev/null || ! declare -F fail >/dev/null; then
    echo "require_readonly_permissions: caller must define ok() and fail() first" >&2
    return 2
  fi

  if grep -qE '^permissions:[[:space:]]*$' "$workflow" \
    && grep -qE '^[[:space:]]+contents:[[:space:]]*read' "$workflow"; then
    ok "permissions block grants only contents: read"
  else
    fail "no 'permissions: contents: read' block — least-privilege required"
  fi
}
