# Gate milestone PRs in the Dependency Review workflow (Issue #402)

## Summary

The standalone Dependency Review workflow (`.github/workflows/dependency-review.yml`)
used a bare `pull_request.branches: ["*"]` filter. In GitHub Actions branch
filters a single-level `*` glob matches only slash-free branch names, so PRs
targeting a `milestone/<slug>` branch never triggered this workflow. Those
sub-issue PRs merged into the shared milestone branch with no dependency-review
gate — a newly introduced vulnerable or disallowed-licence dependency only
surfaced later on the single rollup PR into `Develop`, where it is harder to
attribute and unwind. This directly undermined the workflow's stated purpose of
running "even on PRs targeting branches that bypass the full CI graph".

This is the same root cause already fixed for the seven sibling gates
(#390–#396); this file was missed.

The fix adds a `milestone/*` glob to the filter so every intermediate milestone
sub-issue PR is gated too. Milestone branch names are `milestone/<slug>` with no
nested slashes, so the single-level `milestone/*` glob is sufficient.

Closes #402.

## Change

```yaml
on:
  pull_request:
    branches: ["*", "milestone/*"]
```

```mermaid
flowchart LR
    A[sub-issue PR<br/>base: milestone/audit-21-july] -->|before: "*" skips slashed refs| B[merged unreviewed]
    A -->|after: milestone/* matches| C[Dependency Review gate runs]
```

The validator `scripts/check-dependency-review-workflow.sh` gained a fifth rule
that fails when a `pull_request` branches filter omits `milestone/*`, and passes
when the filter is dropped entirely (a bare `pull_request` trigger runs on every
PR target — the alternative fix noted in the issue).

## Evidence

Backend/CI change — no web interface to screenshot. Verified via:

- New regression test `tests/scripts/dependency_review_workflow.bats::fails when
  the branches filter omits milestone/*` — fails against the unfixed `["*"]`
  filter (confirmed by temporarily reverting the validator) and passes after the
  fix.
- New test `...::passes when the pull_request branches filter is dropped
  entirely` covers the equivalent bare-`pull_request` fix.
- `bats tests/scripts/dependency_review_workflow.bats` — all 12 pass; the full
  `tests/scripts` suite (327 tests) still passes.
- `shellcheck scripts/check-dependency-review-workflow.sh`, `bash -n`, and
  `actionlint .github/workflows/dependency-review.yml` all clean.
- `./scripts/check-dependency-review-workflow.sh` against the real workflow
  reports 5 `OK` markers, including "milestone PRs are gated".
- `./scripts/spell-check.sh` (codespell) — no typos.

The Rust workspace was not rebuilt: this change touches only workflow YAML, a
shell validator, and its BATS test — no Rust source is affected.

## Test Plan

- Added `tests/scripts/dependency_review_workflow.bats::fails when the branches
  filter omits milestone/*` (regression for the fix).
- Added `tests/scripts/dependency_review_workflow.bats::passes when the
  pull_request branches filter is dropped entirely`.
- Updated the canonical fixture and OK-marker count (4 → 5) to reflect the new
  rule.
- Ran `bats tests/scripts/dependency_review_workflow.bats` and the full
  `tests/scripts` suite — all pass.
