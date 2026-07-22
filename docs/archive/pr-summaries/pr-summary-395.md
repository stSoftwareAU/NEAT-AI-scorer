## Summary

The Markdown Lint CI quality workflow (`.github/workflows/markdown-lint.yml`)
used a bare `["*"]` `pull_request.branches` filter. A single-level `"*"` glob
matches only slash-free branch names, so milestone sub-issue PRs targeting
`milestone/<slug>` never triggered the workflow — they merged into the milestone
branch **unlinted**, and the gap only surfaced later at the single rollup PR into
the default branch.

This PR adds `milestone/*` to the filter (`["*", "milestone/*"]`) so every
intermediate milestone PR is linted too, mirroring the existing #393 (ci.yml)
and #394 (gitleaks.yml) fixes. Closes #395.

```mermaid
flowchart LR
    subgraph Before["Before — bare [\"*\"]"]
        A1[PR → milestone/audit-21-july] -.->|glob '*' skips slash names| B1((Markdown Lint\nnever runs))
    end
    subgraph After["After — [\"*\", \"milestone/*\"]"]
        A2[PR → milestone/audit-21-july] -->|milestone/* matches| B2((Markdown Lint\nruns & gates))
    end
```

## Evidence

Backend/CI-config change — no web interface to screenshot. Verified via the
existing milestone branch-filter validator and BATS suite:

- `./scripts/check-milestone-branch-filter.sh --workflow .github/workflows/markdown-lint.yml`
  now prints `OK ... includes 'milestone/*'` (previously
  `FAIL ... must include 'milestone/*' ... (found: * )`).
- `./scripts/check-markdown-lint-workflow.sh` still passes every rule (7 `OK`
  markers), confirming the added glob did not regress the other markdown-lint
  gate assertions.

## Test Plan

- Added `tests/scripts/milestone_branch_filter.bats::"the repository Markdown
  Lint workflow gates milestone PRs"` — a regression test that runs the real
  validator against `.github/workflows/markdown-lint.yml`. It failed before the
  filter change and passes after.
- Ran the full `tests/scripts/milestone_branch_filter.bats` (9 tests) and
  `tests/scripts/markdown_lint_workflow.bats` (21 tests) suites — all pass; the
  markdown-lint fixture-based suite is unaffected because it uses its own inline
  fixture.
