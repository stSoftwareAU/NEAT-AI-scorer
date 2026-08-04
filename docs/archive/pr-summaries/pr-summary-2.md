## Summary

Added a Semgrep SAST scanning GitHub Actions workflow at `.github/workflows/semgrep.yml`. The workflow runs on every pull request (all branches) inside the official `semgrep/semgrep` container and executes `semgrep ci --config p/default`, authenticated via the `SEMGREP_APP_TOKEN` repository secret. This improves the repository's security posture by adding automated static analysis alongside the existing Gitleaks and cargo-audit / dependency-review gates. Closes #2.

## Evidence

This is a CI-only change — no runtime code or UI is affected, so no screenshots or benchmarks apply.

- `./quality.sh`'s shellcheck stage passes (the only stage that can exercise this change). The later Rust stages fail locally because the sibling `NEAT-AI-core` path dependency is not cloned in this environment; that failure is pre-existing and unrelated to this YAML-only addition.
- The workflow mirrors the structure of the existing `.github/workflows/gitleaks.yml` (same trigger shape and minimal `contents: read` permissions) and matches the template supplied in the issue exactly.
- Once merged, Semgrep will run on subsequent pull requests and surface findings via the PR checks UI.

## Test Plan

- [ ] On merge, confirm the `Semgrep` workflow appears under the repository's Actions tab.
- [ ] Open a follow-up PR and confirm the `Semgrep SAST Scanning` check runs and reports results.
- [ ] Verify the `SEMGREP_APP_TOKEN` secret is configured in repository settings (required for Semgrep Cloud features; the job still runs without it using the default ruleset).
