## Summary

Upgraded the GitHub Actions versions used across every workflow to majors that
run on the Node 24 runtime, and added a `quality.sh`-wired validator so the
policy is enforced locally before CI. Two upstream actions (`rustsec/audit-check`
and `actions/dependency-review-action`) have no Node 24 release yet, so they are
documented as tracked Node 20 exceptions in both the validator and the README.

Closes #24.

### Action upgrades

| Action | Before | After | Notes |
| --- | --- | --- | --- |
| `actions/checkout` | `@v4` | `@v5` | Node 24 runtime |
| `actions/cache` | `@v4` | `@v5` | Node 24 runtime |
| `peter-evans/create-pull-request` | `@v7` | `@v8` | Node 24 runtime |
| `ludeeus/action-shellcheck` | `@master` | `@2.0.0` | Pinned to a tagged release |
| `actions/dependency-review-action` | `@v4` | `@v4` | Tracked Node 20 exception (no v5 upstream) |
| `rustsec/audit-check` | `@v2` | `@v2` | Tracked Node 20 exception (no v3 upstream) |
| `dtolnay/rust-toolchain` | `@stable` | `@stable` | Composite/shell action — no Node runtime |

### Policy enforcement

Added `scripts/check-workflow-action-versions.sh` which scans every
`.github/workflows/*.yml` file and classifies each `uses:` reference against a
policy table: required minimum major, tracked Node 20 exception (must stay on
exactly the listed major until upstream ships a Node 24 release), or
composite/shell action (policy not applicable). Unknown actions produce a
non-failing WARN so future additions are flagged for review.

`quality.sh` now calls the script so a regression — for example, accidentally
bumping a `ludeeus/action-shellcheck@master` reference back to a branch ref, or
pinning `actions/checkout@v4` on a new workflow — fails locally before it
reaches CI.

## Evidence

This is a CI/tooling change with no web interface to screenshot. The evidence
is the full local quality gate running clean against the upgraded workflows
and the new validator:

```text
$ ./quality.sh < /dev/null
...
🔢 Validating workflow action versions for Node 24 compat (Issue #24)...
OK   .../.github/workflows/auto-format.yml:37: actions/checkout@v5 (>= v5)
OK   .../.github/workflows/ci.yml:98:  actions/cache@v5 (>= v5)
OK   .../.github/workflows/ci.yml:229: ludeeus/action-shellcheck@2.0.0 (>= v2)
OK   .../.github/workflows/security.yml:51: rustsec/audit-check@v2 (Node 20 exception, tracked)
OK   .../.github/workflows/security.yml:65: actions/dependency-review-action@v4 (Node 20 exception, tracked)
OK   .../.github/workflows/upgrade-dependencies.yml:90: peter-evans/create-pull-request@v8 (>= v8)
...
✅ All quality checks passed!
```

`bats tests/scripts/workflow_action_versions.bats` — 13 tests, all pass. The
full `bats tests/scripts/` suite runs 73 tests, all pass.

## Test Plan

- Added `tests/scripts/workflow_action_versions.bats` (13 cases) driving the
  validator against synthetic workflow fixtures. Coverage includes:
  - A compliant workflow passes.
  - Regressions on each `required:` action (checkout < v5, cache < v5,
    create-pull-request < v8, shellcheck on `@master`) fail with a specific
    message naming the offending action + ref.
  - Bumping a Node 20 exception to an unknown major (e.g.
    `rustsec/audit-check@v3`) fails so new majors are not silently adopted.
  - Unknown actions emit a `WARN` without failing.
  - Comments-containing-uses, reusable-workflow calls, missing directories,
    empty directories, and unknown flags are handled correctly.
  - The real repository workflows satisfy the full Node 24 compat policy.
- Full workflow-helper suite (`bats tests/scripts/`) — 73 / 73 pass.
- Full Rust workspace quality gate (`./quality.sh`) — green.
