# PR Summary — Issue #391

## Summary

The standalone Cargo Audit CI workflow (`.github/workflows/cargo-audit.yml`)
guarded its `pull_request` trigger with a bare `branches: ["*"]` glob. In GitHub
Actions branch filters `*` matches any character **except** `/`, so it never
matched milestone sub-issue branches named `milestone/<slug>`. Those PRs merged
into the shared milestone branch without the cargo-audit gate ever running — the
gap only surfaced later at the single rollup PR into the default branch.

Added `milestone/**` to the filter so the gate runs on milestone PRs too,
mirroring the earlier actionlint fix (Issue #390). The workflow validator
`scripts/check-cargo-audit-workflow.sh` now enforces the milestone coverage as a
new rule, and a BATS regression test locks the behaviour in. Closes #391.

```mermaid
flowchart LR
    A[Milestone sub-issue PR<br/>base: milestone/slug] -->|before: "*" skips slashes| B[cargo-audit SKIPPED]
    A -->|after: "*", "milestone/**"| C[cargo-audit RUNS ✅]
```

## Evidence

Backend/CI change only — no web interface to screenshot. Verified via the
workflow validator and its BATS suite.

Validator against the real workflow (7/7 rules pass, including the new one):

```text
OK   .../cargo-audit.yml: pull_request branch filter covers milestone/* branches
```

BATS suite (`tests/scripts/cargo_audit_workflow.bats`) — all 12 tests pass,
including the new milestone regression test:

```text
ok 1 passes on the canonical fixture
ok 9 fails when the pull_request branch filter omits milestone branches
...
ok 12 real repository cargo-audit workflow satisfies every rule
```

`./quality.sh` passes cleanly (shellcheck, cargo-deny, fmt, clippy, check,
build, test, doc, release build).

## Test Plan

- Updated `tests/scripts/cargo_audit_workflow.bats`:
  - Canonical + rustsec fixtures now use `branches: ["*", "milestone/**"]`.
  - `passes on the canonical fixture` asserts 7 `OK` rules (was 6).
  - Added `fails when the pull_request branch filter omits milestone branches`,
    which reverts the fixture to the bare `"*"` filter and asserts a non-zero
    exit mentioning `milestone` — reproduces the #391 gap and passes after the
    fix.
- `scripts/check-cargo-audit-workflow.sh` gained rule 7 enforcing the
  `milestone/*` filter, so any future regression of the real workflow is caught
  by the existing `real repository cargo-audit workflow satisfies every rule`
  test.
