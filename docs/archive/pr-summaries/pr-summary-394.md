## Summary

The Gitleaks secret-scanning workflow (`.github/workflows/gitleaks.yml`) gated
pull requests with `on.pull_request.branches: ["*"]`. GitHub's `*` glob does not
span `/`, so it matched only top-level branches — `milestone/<slug>` sub-issue
PRs never triggered the scan and merged into the milestone branch unscanned. The
gap only surfaced later at the single rollup PR into the default branch.

This PR adds the single-level `milestone/*` glob to the filter so the
secret-scanning gate runs on every milestone sub-issue PR, matching the existing
`ci.yml` treatment (Issue #393). The invariant is now enforced locally and in CI
by reusing `scripts/check-milestone-branch-filter.sh` against `gitleaks.yml`.

Closes #394.

## Evidence

This is a CI/workflow change with no web interface to screenshot. It is verified
by the milestone branch-filter validator and its BATS suite.

Before — the validator failed against `gitleaks.yml`:

```text
FAIL .github/workflows/gitleaks.yml: pull_request.branches must include 'milestone/*' ... (found: * )
```

After — the validator passes:

```text
OK   .github/workflows/gitleaks.yml: pull_request.branches includes 'milestone/*' — milestone PRs are gated
```

Branch-matching behaviour before and after the fix:

```mermaid
flowchart LR
    subgraph before["Before: branches: [\"*\"]"]
        A1[PR -> Develop] --> G1[Gitleaks runs]
        A2[PR -> milestone/audit-21-july] -.skipped.-> X1[No scan]
    end
    subgraph after["After: branches: [\"*\", \"milestone/*\"]"]
        B1[PR -> Develop] --> G2[Gitleaks runs]
        B2[PR -> milestone/audit-21-july] --> G3[Gitleaks runs]
    end
```

## Test Plan

- Added `tests/scripts/milestone_branch_filter.bats::the repository Gitleaks
  workflow gates milestone PRs` — runs the validator against the real
  `.github/workflows/gitleaks.yml`; fails on the old `["*"]` filter and passes
  after the `milestone/*` glob is added.
- Existing `milestone_branch_filter.bats` and `gitleaks_workflow.bats` suites
  continue to pass.
- `quality.sh` now invokes the validator against `gitleaks.yml` so the gate is
  enforced on every quality run and in CI.
