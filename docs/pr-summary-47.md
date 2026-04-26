## Summary

Completes the Semgrep SAST Scanning workflow by hardening the existing
container-based configuration and adding a validator that enforces the
hardening rules. The workflow now explicitly documents that the
`semgrep/semgrep` container path is the equivalent of the
`semgrep/semgrep-action` GitHub Action, satisfying the workflow-sync
detector that flagged the missing reference. Closes #47.

### What changed

- **`.github/workflows/semgrep.yml`** — pinned the container image to
  `semgrep/semgrep:1.86.0` (was unpinned `semgrep/semgrep`) and added a
  rationale comment block that explains the container approach is the
  equivalent of `semgrep/semgrep-action`. Trigger, permissions, checkout,
  CLI invocation, and `SEMGREP_APP_TOKEN` wiring are unchanged.
- **`scripts/check-semgrep-workflow.sh`** — new validator that enforces
  seven rules: `pull_request` trigger, least-privilege permissions, pinned
  Semgrep entry point (container tag or `semgrep/semgrep-action@vN`),
  `semgrep ci|scan` invocation with explicit `--config <ruleset>`,
  `SEMGREP_APP_TOKEN` sourced from secrets, and a comment documenting the
  container/action equivalence.
- **`tests/scripts/semgrep_workflow.bats`** — 14 BATS cases covering both
  hardened fixtures (container and action paths), each rule's failure
  mode, missing-file and unknown-flag handling, and a real-workflow
  assertion against `.github/workflows/semgrep.yml`.
- **`quality.sh`** — runs the new validator alongside the existing
  Gitleaks workflow check.
- **`Cargo.lock`** — incidental sync to `rust_scorer 0.5.17` from
  running `cargo update` during the quality gate.

### Why the container path?

The `semgrep/semgrep` container is upstream's recommended PR-scan
integration and is functionally equivalent to the
`semgrep/semgrep-action` GitHub Action: both consume `SEMGREP_APP_TOKEN`
from repo secrets and run `semgrep ci --config <ruleset>`. The container
path simply executes the CLI directly inside a pinned image instead of
through the action wrapper, which gives us:

1. **Reproducibility** — every PR runs the same scanner version.
2. **Transparency** — bumping Semgrep is an explicit, reviewable change to
   the `image:` tag.
3. **Fewer marketplace dependencies** — consistent with the project's
   Gitleaks pattern (Issue #21), which prefers pinned binaries over
   marketplace actions where practical.

The validator accepts either the container path or a numeric-major pin of
`semgrep/semgrep-action@vN`, so a future switch back to the action is
still allowed without script changes.

## Evidence

Backend / CI change — no UI to screenshot. Verified via:

1. `./quality.sh < /dev/null` — passes cleanly end-to-end (shellcheck,
   workflow validators including the new Semgrep check, codespell,
   bats, cargo-deny, fmt, clippy, check, build, test, doc, release).
2. `bats tests/scripts/semgrep_workflow.bats` — 14/14 tests passing
   (both hardened fixtures pass; mutated fixtures each fail on the
   expected rule; real workflow satisfies every rule).
3. Direct run: `./scripts/check-semgrep-workflow.sh` → all seven rules
   report `OK`.

## Test Plan

- Added `tests/scripts/semgrep_workflow.bats` covering:
  - Container fixture passes validation.
  - `semgrep/semgrep-action` fixture passes validation.
  - Fails when the workflow is not triggered on `pull_request`.
  - Fails when the `permissions: contents: read` block is missing.
  - Fails when the container image is unpinned (no tag).
  - Fails when the container image is pinned to `:latest`.
  - Fails when no Semgrep entry point is present.
  - Fails when `semgrep/semgrep-action` is pinned to a branch ref.
  - Fails when `--config` is missing from the container CLI invocation.
  - Fails when `SEMGREP_APP_TOKEN` is not wired from secrets.
  - Fails when the `semgrep/semgrep-action` equivalence comment is missing.
  - Reports an error when the workflow file does not exist.
  - Unknown flag prints usage and exits non-zero.
  - Real repository `semgrep.yml` satisfies every rule.
- Existing BATS suites continue to pass.
- Full `./quality.sh` re-run: fmt / clippy / check / build / test / doc /
  release all green.
