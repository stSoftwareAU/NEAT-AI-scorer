#!/usr/bin/env bats
# Regression test for Issue #379 — the `shell-checks` job's NEAT-AI-core
# path-dependency checkout must not persist the workflow GITHUB_TOKEN on disk.
#
# `actions/checkout` writes the token into `.git/config` by default. The
# `shell-checks` job only reads the tree (lint/syntax/bats), never pushes back,
# so the credential must be disabled with `persist-credentials: false`.
#
# Parses the real ci.yml: isolates the `shell-checks:` job, finds the checkout
# step whose `with:` targets `stSoftwareAU/NEAT-AI-core`, and asserts that same
# step sets `persist-credentials: false`. Behaviour is asserted against the
# shipped workflow so the fix and the file cannot drift apart.

setup() {
  CI_WF="${BATS_TEST_DIRNAME}/../../.github/workflows/ci.yml"
  export CI_WF
}

# Emit "yes" iff, inside the `shell-checks` job, the checkout step that targets
# repository stSoftwareAU/NEAT-AI-core also sets persist-credentials: false.
neat_core_checkout_disables_persist() {
  local file="$1"
  awk '
    # Track entry/exit of the shell-checks job (2-space indented job key).
    /^  shell-checks:[[:space:]]*$/ { in_job = 1; next }
    /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { in_job = 0 }
    in_job == 0 { next }

    # A new step (6-space "- ...") closes the previous step scope.
    /^      - / {
      is_checkout = ($0 ~ /uses:[[:space:]]*actions\/checkout/) ? 1 : 0
      is_neat = 0
      has_disable = 0
    }
    /uses:[[:space:]]*actions\/checkout/ { is_checkout = 1 }
    is_checkout && /repository:[[:space:]]*stSoftwareAU\/NEAT-AI-core/ { is_neat = 1 }
    is_checkout && /persist-credentials:[[:space:]]*false/ { has_disable = 1 }
    is_checkout && is_neat && has_disable { found = 1 }

    END { print (found ? "yes" : "no") }
  ' "$file"
}

@test "shell-checks NEAT-AI-core checkout sets persist-credentials: false" {
  [ -f "$CI_WF" ]
  run neat_core_checkout_disables_persist "$CI_WF"
  [ "$status" -eq 0 ]
  [ "$output" = "yes" ]
}

@test "parser reports 'no' when the disable flag is absent (guards the assertion)" {
  local tmp
  tmp="$(mktemp)"
  cat >"$tmp" <<'EOF'
jobs:
  shell-checks:
    steps:
      - name: Checkout NEAT-AI-core
        uses: actions/checkout@sha
        with:
          repository: stSoftwareAU/NEAT-AI-core
          ref: Develop
          path: NEAT-AI-core
  spell-check:
    steps:
      - run: echo ok
EOF
  run neat_core_checkout_disables_persist "$tmp"
  rm -f "$tmp"
  [ "$output" = "no" ]
}
