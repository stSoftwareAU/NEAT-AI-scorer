## Summary
Adds an auto-format PR job (`.github/workflows/auto-format.yml`) that runs `cargo fmt --all` on PR branches targeting `Develop` or `milestone/**`. When rustfmt produces changes, the job commits them with a deterministic message and pushes back to the PR branch; when the tree is already clean, the commit step is skipped, so re-running is a no-op. Closes #19.

## Design
- **Helper script**: `scripts/auto-format.sh` exposes two testable subcommands — `--commit-message` (deterministic string referencing issue #19) and `--check-changes` (exit 0 when the working tree is dirty, 1 when clean via `git status --porcelain`). Cargo invocations stay in the workflow, so the BATS suite needs no Rust toolchain.
- **Workflow**: minimal permissions (`contents: write`, `pull-requests: read`), skips fork PRs (GITHUB_TOKEN cannot push to forks), and reuses the existing NEAT-AI-core sibling-checkout strategy so `cargo fmt --all` resolves the path dependency.
- **Workflow validator**: `scripts/check-auto-format-workflow.sh` enforces the acceptance criteria (PR-only trigger, minimal permissions, `cargo fmt --all` invocation, conditional commit, fork guard, strict bash) and is run by `quality.sh` alongside the other workflow validators.

## Evidence
Backend/CI change — no UI to screenshot. Verified via:
- `bats tests/scripts/auto_format.bats` — 13/13 pass, including fixtures that exercise every validator failure mode plus the shipped `auto-format.yml`.
- `./quality.sh` — passes cleanly (shellcheck, workflow path check, Gitleaks validator, auto-format validator, codespell, bats, cargo-deny, fmt, clippy, check, build, tests, doc, release).

## Test Plan
- `tests/scripts/auto_format.bats` — new BATS suite covering:
  - `--commit-message` is deterministic and mentions rustfmt + issue #19.
  - `--check-changes` exits 1 on a clean git repo, exits 0 on modified tracked files, exits 0 when a new untracked file appears.
  - Usage errors (unknown flag, missing mode) fail non-zero and print usage.
  - Workflow validator passes on a well-formed fixture.
  - Workflow validator fails when: `pull_request` trigger missing, `write-all` permissions, no `cargo fmt --all`, commit step unconditional, fork-PR guard missing.
  - Shipped `.github/workflows/auto-format.yml` validates cleanly.
