## Summary

Added the official `rustsec/audit-check@v2` step to `.github/workflows/security.yml` so the Cargo Security Audit workflow satisfies all expected detection patterns (`cargo audit`, `cargo-audit`, and `rustsec/audit-check`). The RustSec action annotates check runs and PRs with any RUSTSEC advisories; the existing manual `cargo install cargo-audit` + `cargo audit` steps remain as a belt-and-braces hard failure gate, and granted `checks: write` / `issues: write` permissions so the action can report findings. Closes #3.

## Evidence

This is a CI-configuration-only change with no runtime code or UI to screenshot. Verification performed:

- Confirmed all three required detection patterns appear in `.github/workflows/security.yml`:
  - `cargo audit` ✓
  - `cargo-audit` ✓
  - `rustsec/audit-check` ✓
- Ran `shellcheck` via `./quality.sh` — all bash scripts pass.
- The cargo quality steps (`cargo check`, `cargo test`, etc.) require the sibling `../NEAT-AI-core` path dependency which is not cloned in this environment (documented in `AGENTS.md`); they are unaffected by a YAML-only change and are exercised by CI on PR.

## Test Plan

- [x] `rustsec/audit-check@v2` step present and configured with `GITHUB_TOKEN`.
- [x] Existing `cargo audit` hard-failure step retained so the reusable workflow still fails a job on any advisory.
- [x] `permissions:` block extended with `checks: write` and `issues: write` for the RustSec action.
- [x] YAML validated via pattern check; shellcheck passes.
- [ ] On next PR, verify the `Cargo Security Audit (rustsec/audit-check)` step runs and annotates the PR.
