# Gate milestone PRs in the Semgrep workflow (Issue #396)

## Summary

The Semgrep SAST CI quality workflow (`.github/workflows/semgrep.yml`) used a
bare `pull_request.branches: ["*"]` filter. A single-level `*` glob matches
only slash-free branch names, so `milestone/<slug>` sub-issue PRs were never
scanned — they merged into the shared milestone branch unchecked, and the gap
only surfaced later at the single rollup PR into the default branch.

The fix adds a `milestone/*` glob to the filter so every intermediate
milestone sub-issue PR is gated too, matching the pattern already applied to
the CI (#393), Gitleaks (#394), and Markdown Lint (#395) workflows.

Closes #396.

## Change

```yaml
on:
  pull_request:
    branches: ["*", "milestone/*"]
```

```mermaid
flowchart LR
    A[sub-issue PR<br/>base: milestone/audit-21-july] -->|before: "*" skips slashed refs| B[merged unscanned]
    A -->|after: milestone/* matches| C[Semgrep gate runs]
```

Milestone branch names are `milestone/<slug>` with no nested slashes, so the
single-level `milestone/*` glob is sufficient.

## Evidence

Backend/CI change — no web interface to screenshot. Verified via the existing
milestone branch-filter validator (`scripts/check-milestone-branch-filter.sh`)
and the BATS suites:

- New regression test `tests/scripts/milestone_branch_filter.bats::the
  repository Semgrep workflow gates milestone PRs` fails against the unfixed
  `["*"]` filter and passes after the fix.
- `tests/scripts/semgrep_workflow.bats` (26 assertions) still passes,
  confirming the added glob does not regress the Semgrep hardening rules.
- `actionlint` accepts the updated workflow.

## Test Plan

- Added `tests/scripts/milestone_branch_filter.bats::the repository Semgrep
  workflow gates milestone PRs`.
- Ran `bats tests/scripts/milestone_branch_filter.bats
  tests/scripts/semgrep_workflow.bats` — all pass.
- Ran `./quality.sh` — passes cleanly.
