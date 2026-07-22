#!/usr/bin/env bats
# Regression test for Issue #381 — the `validation` job's primary "Checkout
# code" step must not persist the workflow GITHUB_TOKEN on disk.
#
# `actions/checkout` writes the token into `.git/config` by default. The
# `validation` job only reads the tree (required-file check + cargo metadata),
# never pushes back, so the credential must be disabled with
# `persist-credentials: false`.
#
# Parses the real ci.yml: isolates the `validation:` job, finds the self
# checkout step (the one with no `repository:` override), and asserts that same
# step sets `persist-credentials: false`. Behaviour is asserted against the
# shipped workflow so the fix and the file cannot drift apart.

setup() {
  CI_WF="${BATS_TEST_DIRNAME}/../../.github/workflows/ci.yml"
  export CI_WF
}

# Emit "yes" iff, inside the `validation` job, the self checkout step (an
# actions/checkout with no `repository:` override) also sets
# persist-credentials: false.
self_checkout_disables_persist() {
  local file="$1"
  awk '
    # Evaluate the just-finished step before opening a new scope.
    function close_step() {
      if (is_checkout && is_self && has_disable) found = 1
    }

    # Track entry/exit of the validation job (2-space indented job key).
    /^  validation:[[:space:]]*$/ { in_job = 1; next }
    /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { if (in_job) { close_step() }; in_job = 0 }
    in_job == 0 { next }

    # A new step (6-space "- ...") closes the previous step scope.
    /^      - / {
      close_step()
      is_checkout = ($0 ~ /uses:[[:space:]]*actions\/checkout/) ? 1 : 0
      is_self = 1            # assume self-checkout until a repository: appears
      has_disable = 0
    }
    /uses:[[:space:]]*actions\/checkout/ { is_checkout = 1 }
    is_checkout && /repository:[[:space:]]/ { is_self = 0 }
    is_checkout && /persist-credentials:[[:space:]]*false/ { has_disable = 1 }

    END { close_step(); print (found ? "yes" : "no") }
  ' "$file"
}

@test "validation self checkout sets persist-credentials: false" {
  [ -f "$CI_WF" ]
  run self_checkout_disables_persist "$CI_WF"
  [ "$status" -eq 0 ]
  [ "$output" = "yes" ]
}

@test "parser reports 'no' when the disable flag is absent (guards the assertion)" {
  local tmp
  tmp="$(mktemp)"
  cat >"$tmp" <<'EOF'
jobs:
  validation:
    steps:
      - name: Checkout code
        uses: actions/checkout@sha
      - name: Checkout NEAT-AI-core
        uses: actions/checkout@sha
        with:
          repository: stSoftwareAU/NEAT-AI-core
          persist-credentials: false
  spell-check:
    steps:
      - run: echo ok
EOF
  run self_checkout_disables_persist "$tmp"
  rm -f "$tmp"
  [ "$output" = "no" ]
}
