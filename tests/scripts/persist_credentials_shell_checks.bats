#!/usr/bin/env bats
# Regression test for Issue #379 — the `shell-checks` job's NEAT-AI-core
# path-dependency checkout must not persist the workflow GITHUB_TOKEN on disk.
#
# `actions/checkout` writes the token into `.git/config` by default. The
# `shell-checks` job only reads the tree (lint/syntax/bats), never pushes back,
# so the credential must be disabled with `persist-credentials: false`.
#
# TEST MODIFICATION (Issue #401): the NEAT-AI-core checkout was extracted from
# ci.yml into the `setup-neat-core` composite action. The #379 guarantee is
# unchanged but now lives at a single source of truth, asserted below in both
# places so the fix and the files cannot drift apart:
#   1. The composite action's NEAT-AI-core checkout sets persist-credentials:
#      false, so every consumer inherits a credential-free clone.
#   2. The `shell-checks` job reaches NEAT-AI-core through that composite (and no
#      longer inlines its own checkout that could omit the flag).

setup() {
  CI_WF="${BATS_TEST_DIRNAME}/../../.github/workflows/ci.yml"
  ACTION_YML="${BATS_TEST_DIRNAME}/../../.github/actions/setup-neat-core/action.yml"
  export CI_WF ACTION_YML
}

# Emit "yes" iff the composite action's checkout of stSoftwareAU/NEAT-AI-core
# sets persist-credentials: false.
composite_neat_core_checkout_disables_persist() {
  local file="$1"
  awk '
    # A new step (indented "- ...") closes the previous step scope.
    /^[[:space:]]*- / {
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

# Emit "yes" iff, inside the `shell-checks` job, the NEAT-AI-core setup uses the
# composite action AND does not inline its own actions/checkout of NEAT-AI-core.
shell_checks_reaches_neat_core_via_composite() {
  local file="$1"
  awk '
    /^  shell-checks:[[:space:]]*$/ { in_job = 1; next }
    /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { in_job = 0 }
    in_job == 0 { next }
    /uses:[[:space:]]*\.\/\.github\/actions\/setup-neat-core/ { uses_composite = 1 }
    /repository:[[:space:]]*stSoftwareAU\/NEAT-AI-core/ { inlines_checkout = 1 }
    END { print (uses_composite && !inlines_checkout ? "yes" : "no") }
  ' "$file"
}

@test "composite setup-neat-core checkout sets persist-credentials: false" {
  [ -f "$ACTION_YML" ]
  run composite_neat_core_checkout_disables_persist "$ACTION_YML"
  [ "$status" -eq 0 ]
  [ "$output" = "yes" ]
}

@test "shell-checks job reaches NEAT-AI-core via the composite, not an inline checkout" {
  [ -f "$CI_WF" ]
  run shell_checks_reaches_neat_core_via_composite "$CI_WF"
  [ "$status" -eq 0 ]
  [ "$output" = "yes" ]
}

@test "composite parser reports 'no' when the disable flag is absent (guards the assertion)" {
  local tmp
  tmp="$(mktemp)"
  cat >"$tmp" <<'EOF'
runs:
  using: composite
  steps:
    - name: Checkout NEAT-AI-core
      uses: actions/checkout@sha
      with:
        repository: stSoftwareAU/NEAT-AI-core
        ref: Develop
        path: NEAT-AI-core
EOF
  run composite_neat_core_checkout_disables_persist "$tmp"
  rm -f "$tmp"
  [ "$output" = "no" ]
}

@test "shell-checks parser reports 'no' when the job re-inlines an NEAT-AI-core checkout (guards the assertion)" {
  local tmp
  tmp="$(mktemp)"
  cat >"$tmp" <<'EOF'
jobs:
  shell-checks:
    steps:
      - name: Set up NEAT-AI-core sibling
        uses: ./.github/actions/setup-neat-core
      - name: Checkout NEAT-AI-core again
        uses: actions/checkout@sha
        with:
          repository: stSoftwareAU/NEAT-AI-core
          path: NEAT-AI-core
  spell-check:
    steps:
      - run: echo ok
EOF
  run shell_checks_reaches_neat_core_via_composite "$tmp"
  rm -f "$tmp"
  [ "$output" = "no" ]
}
