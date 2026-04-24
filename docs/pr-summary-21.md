## Summary

Hardens the Gitleaks PR-scan workflow so every run is reproducible, supply-chain verifiable, and scoped to the PR diff range. Closes #21.

### What changed

- **`.github/workflows/gitleaks.yml`** — rewritten with pinned binary install, SHA256 checksum verification, explicit `git fetch` of the PR base ref, `set -euo pipefail` in every `run:` block, and an inline comment explaining why the version is pinned. Scan scope stays at `origin/<base>..HEAD` so only the PR commit range is evaluated. Strict failure semantics are preserved — Gitleaks exits non-zero on any finding and the job fails.
- **`scripts/check-gitleaks-workflow.sh`** — new validator that enforces the hardening rules against the workflow file. Designed for reuse from `quality.sh` and the bats suite; fails fast with a descriptive message when a rule regresses.
- **`tests/scripts/gitleaks_workflow.bats`** — new 12-case bats suite covering every rule (each mutated fixture triggers the matching failure) plus a real-workflow assertion.
- **`quality.sh`** — runs the new validator alongside the existing workflow-path check.
- **`.codespellrc`** — excludes `docs/pr-summary-*.md` from codespell. These files quote typo fixtures from the PRs they describe (Issue #22 introduced this pattern) and were breaking every subsequent PR's quality gate.

### Pinned version rationale

Gitleaks is pinned to **v8.24.2** with SHA256 `fa0500f6b7e41d28791ebc680f5dd9899cd42b58629218a5f041efa899151a8e`. Pinning gives us:

1. **Reproducibility** — a given PR always executes the same scanner binary, so rule updates never silently change PR-gating behaviour.
2. **Supply-chain hygiene** — the downloaded archive is verified against a known checksum, so a compromised release asset cannot silently replace the scanner.
3. **Transparency** — bumping Gitleaks is an explicit, reviewable change to both `GITLEAKS_VERSION` and `GITLEAKS_SHA256`.

### Acceptance criteria

- ✅ Replace/augment `gitleaks-action` with pinned binary install — direct release-tarball download, verified by SHA256.
- ✅ Scan `origin/<base>..HEAD` in PR context — `--log-opts="origin/${{ github.base_ref }}..HEAD"` plus an explicit `git fetch origin <base_ref>` step so the ref resolves locally.
- ✅ Keep logs actionable and failure semantics strict — `set -euo pipefail` in every run block, `--verbose --redact`, Gitleaks' non-zero exit on findings propagates to the job status.
- ✅ Workflow documents the pinned version and why it is pinned — header comment block explains reproducibility, supply-chain hygiene, and the bump protocol.

## Evidence

Backend / CI change — no UI to screenshot. Verified via:

1. `./quality.sh < /dev/null` — passes cleanly end-to-end (shellcheck, workflow validators, codespell, bats, cargo-deny, fmt, clippy, check, build, test, doc, release).
2. `bats tests/scripts/gitleaks_workflow.bats` — 12/12 tests passing (hardened fixture passes; mutated fixtures each fail on the expected rule; real workflow satisfies every rule).
3. Direct run: `./scripts/check-gitleaks-workflow.sh` → all nine hardening rules report `OK`.

## Test Plan

- Added `tests/scripts/gitleaks_workflow.bats` covering:
  - Hardened fixture passes validation.
  - Fails when the workflow uses `gitleaks-action` instead of a pinned binary.
  - Fails when the pinned release URL is missing.
  - Fails when there is no comment documenting the pin rationale.
  - Fails when the archive checksum is not verified.
  - Fails when the scan does not limit to `base..HEAD`.
  - Fails when the base ref is not explicitly fetched.
  - Fails when strict bash is missing from run blocks.
  - Fails when the workflow does not invoke `gitleaks detect`.
  - Reports an error when the workflow file does not exist.
  - Unknown flag prints usage and exits non-zero.
  - Real repository `gitleaks.yml` satisfies every hardening rule.
- Existing bats suites (`spell_check.bats`, `version_increment.bats`, `workflow_neat_ai_core_path.bats`) continue to pass.
- Full `./quality.sh` gate re-run: fmt / clippy / check / build / test / doc / release all green.
