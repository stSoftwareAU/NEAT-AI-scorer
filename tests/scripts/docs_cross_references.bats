#!/usr/bin/env bats
# Tests for scripts/check-docs-cross-references.sh — Issue #505.
#
# The validator keeps cross-document citations alive: the canonical rule
# sections must exist in CONTRIBUTING.md, every `](target.md#anchor)` link must
# resolve to a real heading, and no document may re-attribute those rules to
# AGENTS.md. Fixtures live in a temp tree so the real documents are never
# mutated by the tests.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-docs-cross-references.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_DIR="$(mktemp -d)"
  export TMP_DIR
  mkdir -p "$TMP_DIR/docs"

  write_contributing
  write_readme
  write_agents
  write_design
}

teardown() {
  rm -rf "$TMP_DIR"
}

run_check() {
  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR"
}

write_contributing() {
  cat >"$TMP_DIR/CONTRIBUTING.md" <<'EOF'
# Contributing

## Performance Task Workflow

Before/after Criterion evidence is mandatory. A miss posts the numbers, is
labelled `negative-result`, and closes `not planned`.

## Human escalation

The worker holds no `workflow` OAuth scope, so workflow YAML needs a maintainer.
EOF
}

write_readme() {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Readme

## How to bench

Per the [Performance Task Workflow](CONTRIBUTING.md#performance-task-workflow),
performance PRs without before/after evidence are rejected.

## PGO

The worker cannot push workflow YAML — see
[Human escalation](CONTRIBUTING.md#human-escalation).
EOF
}

write_agents() {
  cat >"$TMP_DIR/AGENTS.md" <<'EOF'
# AGENTS.md

- Rules live in [Performance Task Workflow](./CONTRIBUTING.md#performance-task-workflow).
EOF
}

# A doc with a heading carrying an em dash, so the double-hyphen GitHub slug is
# exercised alongside the ordinary case.
write_design() {
  cat >"$TMP_DIR/docs/gpu-scoring-design.md" <<'EOF'
# Design

See [baseline](performance-baseline.md#hot-spots--9-may-2026-issue-79) and the
[Performance Task Workflow](../CONTRIBUTING.md#performance-task-workflow).
EOF
  cat >"$TMP_DIR/docs/performance-baseline.md" <<'EOF'
# Baseline

### Hot spots — 9 May 2026 (Issue #79)

Numbers.
EOF
}

@test "passes when every citation resolves" {
  run_check
  [ "$status" -eq 0 ]
  [[ "$output" == *"citations resolve"* ]]
}

@test "fails when a cited anchor does not exist in the target" {
  cat >>"$TMP_DIR/README.md" <<'EOF'

See [missing](CONTRIBUTING.md#no-such-section).
EOF
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"dead anchor '#no-such-section'"* ]]
}

@test "fails when the cited target file does not exist" {
  cat >>"$TMP_DIR/README.md" <<'EOF'

See [gone](GHOST.md#anything).
EOF
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"link target does not exist"* ]]
}

@test "fails when CONTRIBUTING loses the Performance Task Workflow section" {
  sed -i.bak 's/^## Performance Task Workflow$/## Benching/' "$TMP_DIR/CONTRIBUTING.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"no '#performance-task-workflow' section"* ]]
}

@test "fails when CONTRIBUTING loses the Human escalation section" {
  sed -i.bak 's/^## Human escalation$/## Escalating/' "$TMP_DIR/CONTRIBUTING.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"no '#human-escalation' section"* ]]
}

@test "fails when a doc re-attributes the Performance Task Workflow to AGENTS.md" {
  cat >>"$TMP_DIR/README.md" <<'EOF'

Per `AGENTS.md`, the Performance Task Workflow rejects unevidenced PRs.
EOF
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"cites AGENTS.md as the home"* ]]
}

@test "fails when a doc re-attributes Human Escalation to AGENTS.md" {
  cat >>"$TMP_DIR/README.md" <<'EOF'

No `workflow` OAuth scope — see `AGENTS.md` "Human Escalation".
EOF
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"cites AGENTS.md as the home"* ]]
}

@test "fails when a doc credits AGENTS.md with the before/after evidence rule" {
  cat >>"$TMP_DIR/README.md" <<'EOF'

Per `AGENTS.md`, performance PRs without before/after Criterion evidence are
rejected.
EOF
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"cites AGENTS.md as the home"* ]]
}

@test "allows AGENTS.md to point at the canonical CONTRIBUTING.md home" {
  cat >>"$TMP_DIR/AGENTS.md" <<'EOF'
- Human escalation is documented in [CONTRIBUTING.md](./CONTRIBUTING.md#human-escalation).
EOF
  run_check
  [ "$status" -eq 0 ]
}

@test "ignores headings inside fenced code blocks" {
  cat >>"$TMP_DIR/CONTRIBUTING.md" <<'EOF'

```bash
# Fake Heading
echo hi
```
EOF
  cat >>"$TMP_DIR/README.md" <<'EOF'

See [fake](CONTRIBUTING.md#fake-heading).
EOF
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"dead anchor '#fake-heading'"* ]]
}

@test "skips frozen pr-summary archives" {
  cat >"$TMP_DIR/docs/pr-summary-42.md" <<'EOF'
# Historical summary

Rejected per `AGENTS.md` "Performance Task Workflow", link
[dead](../AGENTS.md#performance-task-workflow).
EOF
  run_check
  [ "$status" -eq 0 ]
}

@test "reports a missing root" {
  run "$SCRIPT_UNDER_TEST" --root "$TMP_DIR/nope"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -eq 2 ]
  [[ "$output" == *"Usage"* ]]
}

@test "the real repository documents satisfy the check" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
