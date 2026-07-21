# CI actionlint gate now runs on milestone PRs

## Summary

The standalone Actionlint quality workflow
(`.github/workflows/actionlint.yml`) filtered `pull_request` events with
`branches: ["*"]`. GitHub's branch-filter glob `*` matches only single-level
refs — it does **not** match refs containing a `/`, so PRs targeting a
`milestone/<slug>` sub-issue branch never triggered the gate. Those
intermediate PRs merged into the milestone branch unlinted, with the gap only
caught later by the single rollup PR into the default branch.

The filter is now `branches: ["*", "milestone/**"]`, restoring the gate on
milestone sub-issue PRs while preserving the existing top-level branch
coverage. `milestone/**` matches the repo's existing convention already used by
`auto-format.yml` and `version-increment.yml`.

The workflow validator `scripts/check-actionlint-workflow.sh` gained a sixth
rule that fails when the `pull_request` branch filter omits `milestone/*`, so
this regression cannot silently return.

Closes #390.

## Evidence

Backend/CI-config change — no web UI to screenshot. Verified via the BATS
suite that drives the validator end-to-end against synthetic fixtures, plus the
full `./quality.sh` gate (which invokes the validator against the real
workflow) passing cleanly.

```mermaid
flowchart LR
    A["milestone/&lt;slug&gt; sub-issue PR"] -->|before: branches ['*']| B["gate skipped<br/>(glob '*' ignores '/')"]
    A -->|after: branches ['*', 'milestone/**']| C["actionlint gate runs"]
    B --> D["merges unlinted"]
    C --> E["regressions caught pre-merge"]
```

## Test Plan

- Added `tests/scripts/actionlint_workflow.bats::"fails when the pull_request
  branch filter omits milestone branches"` — reverts the fixture to the bare
  `["*"]` filter and asserts the validator exits non-zero with a `milestone`
  message (reproduces #390 against the unfixed config).
- Updated the canonical-fixture test to expect **6** `OK` rule markers (the new
  milestone rule) and the fixture/replace strings for the new branch filter.
- `tests/scripts/actionlint_workflow.bats::"real repository actionlint workflow
  satisfies every rule"` confirms the shipped `actionlint.yml` now passes the
  milestone rule.
- Full suite: `bats tests/scripts/actionlint_workflow.bats` → 11/11 pass.
- `./quality.sh` → all quality checks passed.
