## Summary
Locks in the in-workspace checkout strategy for `stSoftwareAU/NEAT-AI-core`
across every workflow, documents the approach, and adds an enforced
regression guard so a `path: ../NEAT-AI-core` regression can never reach
`main` again. Closes #18.

`actions/checkout` rejects any `path:` that resolves outside
`$GITHUB_WORKSPACE` (the symptom in the original run:
`Repository path '/home/runner/work/NEAT-AI-scorer/NEAT-AI-core' is not
under '/home/runner/work/NEAT-AI-scorer/NEAT-AI-scorer'`). The fix landed
earlier on Develop — we now clone into `NEAT-AI-core/` inside the workspace
and symlink `$GITHUB_WORKSPACE/../NEAT-AI-core → $GITHUB_WORKSPACE/NEAT-AI-core`
so Cargo still resolves the `../../NEAT-AI-core/neat-core` path dependency
from `rust_scorer/Cargo.toml` without any manifest rewriting.

Changes in this PR:
- Added a machine-checked explanation of the path strategy to every workflow
  that clones NEAT-AI-core (`ci.yml` × 2 jobs, `security.yml`,
  `upgrade-dependencies.yml`). Acceptance criterion: "Documentation/comments
  in workflows explain the path strategy."
- Added `scripts/check-workflow-paths.sh`: parses every workflow, rejects any
  checkout of `stSoftwareAU/NEAT-AI-core` with a `..` or absolute `path:`,
  and requires the sibling-link step so Cargo resolves locally.
- Wired the validator into `quality.sh` so a future regression fails the
  local gate and CI before it ever reaches a runner.
- Added `tests/scripts/workflow_neat_ai_core_path.bats` with 9 BATS cases
  (happy path against the shipped workflows + 8 failure-mode fixtures).
- Cross-referenced the CI strategy from `rust_scorer/Cargo.toml` so future
  readers understand why the path looks the way it does.

## Evidence
CLI change only — no UI. Evidence for each acceptance criterion:

1. **"CI, validation, security, and dependency-upgrade workflows no longer
   fail on checkout path errors."**
   The last CI run on `Develop` (`24880956776`, 2026-04-24) completed
   successfully after the path fix shipped in PR #25. The prior failed run
   (`24880044634`) showed the exact error this issue describes:
   `Repository path '/home/runner/work/NEAT-AI-scorer/NEAT-AI-core' is not
   under '/home/runner/work/NEAT-AI-scorer/NEAT-AI-scorer'`.
2. **"A PR run reaches build/test steps (not blocked during checkout)."**
   Same run — the `Quality Checks` job reached `cargo build`, `cargo test`,
   `cargo doc`, and `cargo deny check` end-to-end.
3. **"Documentation/comments in workflows explain the path strategy."**
   See the updated comment blocks in `.github/workflows/ci.yml`,
   `.github/workflows/security.yml`, and
   `.github/workflows/upgrade-dependencies.yml`.

Quality gate output (excerpt — full run passed):
```
🔗 Validating NEAT-AI-core checkout path strategy in workflows...
OK   .github/workflows/ci.yml: NEAT-AI-core checkout path='NEAT-AI-core'
OK   .github/workflows/ci.yml: NEAT-AI-core checkout path='NEAT-AI-core'
OK   .github/workflows/security.yml: NEAT-AI-core checkout path='NEAT-AI-core'
OK   .github/workflows/upgrade-dependencies.yml: NEAT-AI-core checkout path='NEAT-AI-core'
🧰 Running bash helper tests (bats)...
ok 11 passes when every workflow uses an in-workspace path and a sibling link step
ok 12 fails when a workflow uses a parent-relative checkout path (../NEAT-AI-core)
ok 13 fails when a workflow uses an absolute checkout path
ok 14 fails when a workflow omits the Cargo sibling-link step
ok 15 fails when a NEAT-AI-core checkout has no explicit path at all
ok 16 ignores workflows that do not check out NEAT-AI-core
ok 17 reports an error when the workflows directory does not exist
ok 18 real repository workflows all satisfy the path strategy
ok 19 unknown flag prints usage and exits non-zero
✅ All quality checks passed!
```

## Test Plan
- `tests/scripts/workflow_neat_ai_core_path.bats` — 9 cases exercising
  `scripts/check-workflow-paths.sh`:
  - Happy path: synthetic good workflow passes.
  - Error: `path: ../NEAT-AI-core` flagged as outside-workspace (the exact
    failure mode from the issue).
  - Error: absolute path (`/opt/NEAT-AI-core`) rejected.
  - Error: missing sibling-link step flagged.
  - Error: checkout with no `path:` at all flagged.
  - Unrelated workflows ignored.
  - Missing workflows directory reports error.
  - Real repository workflows: all pass.
  - Unknown flag prints usage.
- `quality.sh` runs the validator and the new BATS suite on every local gate
  run, and CI already invokes the bats suite via `ci.yml`'s `shell-checks`
  job, so a regression is caught both locally and in CI.
- Full `./quality.sh` passes cleanly on this branch (shellcheck, cargo-deny,
  fmt, clippy, check, build, 35 Rust unit + integration tests, doc, release
  build).
