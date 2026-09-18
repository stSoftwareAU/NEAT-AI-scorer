#!/usr/bin/env bats
# Regression test for Issue #379 — the `shell-checks` job must not persist the
# workflow GITHUB_TOKEN on disk.
#
# `actions/checkout` writes the token into `.git/config` by default. The
# `shell-checks` job only reads the tree (lint/syntax/bats), never pushes back,
# so the credential must be disabled with `persist-credentials: false`.
#
# TEST MODIFICATION (Issue #401): the NEAT-AI-core checkout was extracted from
# ci.yml into the `setup-neat-core` composite action, and the #379 guarantee
# was asserted there.
#
# TEST MODIFICATION (Issue #630): `neat-core` is pinned to a NEAT-AI-core
# release tag, so there is no sibling checkout left to keep credential-free and
# the composite action is retired. The guarantee now has one subject again —
# the job's own checkout — and the second assertion becomes the stronger one:
# the job reaches for NO NEAT-AI-core clone at all. The two cases that parsed
# the composite action's YAML are gone with the file they parsed.

setup() {
  CI_WF="${BATS_TEST_DIRNAME}/../../.github/workflows/ci.yml"
  export CI_WF
}

# Emit "yes" iff every actions/checkout inside the `shell-checks` job sets
# persist-credentials: false.
shell_checks_checkouts_disable_persist() {
  local file="$1"
  awk '
    function close_step() {
      if (is_checkout) {
        seen++
        if (!has_disable) leaked++
      }
    }
    /^  shell-checks:[[:space:]]*$/ { in_job = 1; next }
    /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { if (in_job) close_step(); in_job = 0 }
    in_job == 0 { next }
    /^      - / {
      close_step()
      is_checkout = ($0 ~ /uses:[[:space:]]*actions\/checkout/) ? 1 : 0
      has_disable = 0
    }
    /uses:[[:space:]]*actions\/checkout/ { is_checkout = 1 }
    is_checkout && /persist-credentials:[[:space:]]*false/ { has_disable = 1 }
    END { close_step(); print (seen > 0 && leaked == 0 ? "yes" : "no") }
  ' "$file"
}

# Emit "yes" iff the `shell-checks` job reaches for no NEAT-AI-core clone —
# neither the retired composite action nor an inline checkout (Issue #630).
shell_checks_needs_no_sibling_clone() {
  local file="$1"
  awk '
    /^  shell-checks:[[:space:]]*$/ { in_job = 1; next }
    /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { in_job = 0 }
    in_job == 0 { next }
    /uses:[[:space:]]*\.\/\.github\/actions\/setup-neat-core/ { reaches = 1 }
    /repository:[[:space:]]*stSoftwareAU\/NEAT-AI-core/ { reaches = 1 }
    END { print (reaches ? "no" : "yes") }
  ' "$file"
}

@test "shell-checks checkouts set persist-credentials: false" {
  [ -f "$CI_WF" ]
  run shell_checks_checkouts_disable_persist "$CI_WF"
  [ "$status" -eq 0 ]
  [ "$output" = "yes" ]
}

@test "shell-checks job needs no NEAT-AI-core sibling clone" {
  [ -f "$CI_WF" ]
  run shell_checks_needs_no_sibling_clone "$CI_WF"
  [ "$status" -eq 0 ]
  [ "$output" = "yes" ]
}

@test "checkout parser reports 'no' when the disable flag is absent (guards the assertion)" {
  local tmp
  tmp="$(mktemp)"
  cat >"$tmp" <<'EOF'
jobs:
  shell-checks:
    steps:
      - name: Checkout code
        uses: actions/checkout@sha
  spell-check:
    steps:
      - run: echo ok
EOF
  run shell_checks_checkouts_disable_persist "$tmp"
  rm -f "$tmp"
  [ "$output" = "no" ]
}

@test "sibling parser reports 'no' when the job re-inlines a NEAT-AI-core checkout (guards the assertion)" {
  local tmp
  tmp="$(mktemp)"
  cat >"$tmp" <<'EOF'
jobs:
  shell-checks:
    steps:
      - name: Checkout code
        uses: actions/checkout@sha
        with:
          persist-credentials: false
      - name: Checkout NEAT-AI-core again
        uses: actions/checkout@sha
        with:
          repository: stSoftwareAU/NEAT-AI-core
          path: NEAT-AI-core
  spell-check:
    steps:
      - run: echo ok
EOF
  run shell_checks_needs_no_sibling_clone "$tmp"
  rm -f "$tmp"
  [ "$output" = "no" ]
}
