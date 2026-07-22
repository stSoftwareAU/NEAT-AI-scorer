# Fix cargo-quality branch filter to cover milestone PRs

## Summary

The standalone Cargo Quality workflow (`.github/workflows/cargo-quality.yml`)
used `pull_request.branches: ["*"]`, which its own comment described as firing
on PRs against "ANY branch". That is not what `*` does: GitHub's `*` glob does
**not** match across `/`, so `["*"]` matches `Develop` and `main` but **not**
`milestone/<slug>` branches. Milestone sub-issue PRs target a shared
`milestone/<name>` branch, so the fmt + clippy gate silently never ran on them —
every intermediate sub-issue PR merged into the milestone branch unchecked, and
the gap only surfaced at the single rollup PR into the default branch.

The fix switches the filter to `["**"]`, which matches any branch including
nested-slash `milestone/<slug>` and stacked-PR branches, realising the
workflow's documented "ANY branch" intent. The issue's suggested
`[Develop, main, milestone/*]` also works, but `**` is the minimal,
intent-preserving change and additionally covers other nested branch names.

A new rule (rule 7) was added to `scripts/check-cargo-quality-workflow.sh` so
the gate now rejects any `pull_request.branches` filter that cannot match
`milestone/<slug>` (i.e. requires `**` or an explicit `milestone/` glob) —
preventing a regression back to a `/`-blind `*`. The README prose was updated to
match.

Closes #392.

## Evidence

Backend/CI change — no web interface to screenshot. Verified via the repo's own
workflow-validation gate and BATS suite.

- `./scripts/check-cargo-quality-workflow.sh` now reports 8 `OK` rules
  (including the new milestone-coverage rule) against the fixed workflow.
- Full BATS suite passes (327 tests), including the two new cases below.

```mermaid
flowchart LR
    subgraph Before["branches: [\"*\"] — * does not cross /"]
        P1[PR into Develop] --> G1[gate runs]
        P2[PR into milestone/foo] -. skipped .-> X[no gate]
    end
    subgraph After["branches: [\"**\"] — ** matches any branch"]
        P3[PR into Develop] --> G2[gate runs]
        P4[PR into milestone/foo] --> G3[gate runs]
    end
```

## Test Plan

- Added `tests/scripts/cargo_quality_workflow.bats::"fails when the branch filter
  skips milestone/<slug> PRs (Issue #392)"` — mutates the fixture back to
  `["*"]` and asserts the gate exits non-zero (reproduces the bug against the
  unfixed filter).
- Added `tests/scripts/cargo_quality_workflow.bats::"passes when the branch
  filter uses an explicit milestone/* glob"` — confirms
  `[Develop, main, milestone/*]` is accepted.
- Updated the canonical fixture to `["**"]` and the `OK`-marker count assertion
  from 7 to 8 to cover the new rule; kept every existing failure case.
- `real repository cargo-quality workflow satisfies every rule` passes against
  the fixed `.github/workflows/cargo-quality.yml`.
