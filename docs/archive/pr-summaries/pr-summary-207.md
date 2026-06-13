## Summary

The `markdown-lint.yml` workflow's `push` trigger listed `branches: [main, master]`,
but this repo's default branch is `Develop` (per `.vibe_default_branch` and
`origin/HEAD`) — neither `main` nor `master` exists. As a result the markdown-lint
`push` trigger never fired; markdown lint only ran on pull requests, never on pushes
to the actual default branch.

This change sets the `push` branches to `[main, master, Develop]`, matching the
existing `actionlint.yml` for symmetry, so markdown lint now runs on pushes to
`Develop`. The legacy `main`/`master` names are kept harmlessly for parity. Closes #207.

```mermaid
flowchart LR
    P[Push to Develop] -->|before: [main, master]| X[Trigger never fires]
    P -->|after: [main, master, Develop]| R[Markdown Lint runs]
```

## Evidence

This is a CI/workflow change with no web interface, so no screenshot applies.
Verification was done via workflow inspection and the validator script:

- `actionlint .github/workflows/markdown-lint.yml` → **OK** (workflow valid).
- `python3 -c "import yaml; yaml.safe_load(...)"` → **YAML valid**.
- `scripts/check-markdown-lint-workflow.sh` (no args, validates the real workflow)
  reports **`OK ... push trigger targets the default branch (Develop)`** and exits 0.

The validator (`scripts/check-markdown-lint-workflow.sh`) gained a new rule (#6)
that captures the first `branches:` line inside the `push:` block and fails unless
it lists `Develop`, guarding against this regression recurring.

### Unrelated pre-existing failure

`./quality.sh` reports one failing Rust test,
`gpu_auto_directory_above_shader_cap_falls_back_to_cpu_cleanly`
(`rust_scorer/tests/directory_mode_tdd.rs`). It is environment-specific (a Metal GPU
is present, so the oversized creature scores on `metal` instead of `cpu-fallback`)
and was confirmed to fail identically with my changes stashed. It is unrelated to
this workflow/bash change and out of scope for #207.

## Test Plan

- Added `tests/scripts/markdown_lint_workflow.bats::"fails when the push trigger omits Develop (Issue #207)"`
  — mutates the canonical fixture back to `[main, master]` and asserts the validator
  exits non-zero with `push trigger does not target Develop`.
- Updated the canonical bats fixture's push branches to `[main, master, Develop]` and
  the "passes on the canonical fixture" assertion to expect
  `push trigger targets the default branch (Develop)`.
- All 12 bats tests pass, including test #12 which runs the validator against the real
  repository workflow.
