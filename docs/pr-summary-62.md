## Summary

Adds the standalone `Dependency Review` GitHub Actions workflow requested by
the workflow sync tracker. The workflow runs
`actions/dependency-review-action@v4` on every pull request against any
branch, diffing the PR's manifest against the base branch and failing the
run if any newly introduced dependency carries a known vulnerability or
disallowed licence — surfacing supply-chain risk before merge.

The reusable `security.yml` workflow already runs the same action inside the
full CI graph (which only fires for PRs into `Develop`); this dedicated
workflow gives feature branches and stacked PRs the same gate without
spinning up CI. Closes #62.

## Evidence

This is a CI / workflow change with no UI surface and no runtime
performance impact. Verification:

- New BATS suite `tests/scripts/dependency_review_workflow.bats` exercises
  the new validator end-to-end (10 tests, all green).
- The validator `scripts/check-dependency-review-workflow.sh` is invoked
  from `quality.sh` and reports OK on the real workflow file.
- Full local quality gate (`./quality.sh`) passes, including shellcheck,
  the existing 148 BATS tests, cargo-deny, fmt, clippy, check, build, test,
  doc, and release build.
- The Node 24 compat policy in
  `scripts/check-workflow-action-versions.sh` already lists
  `actions/dependency-review-action` as a tracked Node 20 exception at
  `@v4`; the new workflow's pin satisfies that policy.

```mermaid
flowchart LR
    PR[Pull Request] --> CI[ci.yml &mdash; Develop only]
    PR --> DR[dependency-review.yml &mdash; any branch]
    CI --> SEC[security.yml &mdash; reusable]
    SEC --> DRA[actions/dependency-review-action@v4]
    DR --> DRA
    DRA -->|advisory or licence| FAIL[fail PR]
    DRA -->|clean| PASS[merge gate green]
```

## Test Plan

- Added `tests/scripts/dependency_review_workflow.bats` covering the
  canonical fixture, every failure rule (missing `pull_request` trigger,
  missing `permissions` block, unpinned/missing `actions/checkout`,
  unpinned/missing `actions/dependency-review-action`), missing-file and
  unknown-flag error paths, plus an end-to-end check against the real
  workflow file.
- Added `scripts/check-dependency-review-workflow.sh` and wired it into
  `quality.sh`.
- Re-ran the full BATS suite (148 tests, all passing) and `./quality.sh`
  (passes cleanly).
