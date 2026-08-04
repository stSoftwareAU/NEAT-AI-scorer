## Summary

Adds a guarded auto-version-increment PR job so each pull request bumps
`rust_scorer/Cargo.toml` exactly once — and never duplicates the commit on
re-runs or when a human has already bumped the version. Closes #20.

- New helper `scripts/version-increment.sh` with four modes:
  `--get-version`, `--bump-patch`, `--already-bumped`, `--run`. The `--run`
  mode is idempotent and short-circuits when the branch version already
  differs from the base ref.
- New workflow `.github/workflows/version-increment.yml` with two jobs and
  explicit ordering: a `guard` job decides whether a bump is needed and a
  `bump` job re-checks and performs the commit/push. Forks are skipped
  because `GITHUB_TOKEN` cannot push to them.
- `quality.sh` and the CI `shell-checks` job now install/run `bats` so the
  helper script stays covered locally and in CI.
- README gained a short "CI" section describing the new behaviour.

## Evidence

This change is backend/CLI and workflow only — no UI to screenshot. Verified
locally via:

- `bats tests/scripts` — 10/10 passing (see the test plan below).
- `./quality.sh` — full gate green (shellcheck, bats, cargo-deny, fmt,
  clippy `-D warnings` + `filter_next`/`collapsible_if`, test, doc,
  release build).
- Smoke against the real repo:
  `./scripts/version-increment.sh --get-version --manifest rust_scorer/Cargo.toml`
  → `0.5.4`; `--already-bumped` against `origin/milestone/ci` → exit 1
  (correctly identifies that no bump has happened yet on the branch).

## Test Plan

New BATS suite `tests/scripts/version_increment.bats` — each test exercises
the script against a real temporary git repository:

- [x] `get_version` reads the Cargo manifest.
- [x] `bump-patch` returns `X.Y.(Z+1)` (both `--dry-run` and written form).
- [x] `already-bumped` exits 0 when the branch has diverged and 1 otherwise.
- [x] `run` performs the bump when no prior bump exists.
- [x] `run` is idempotent — a second invocation reports `skip:` and does
      not mutate the manifest.
- [x] `run` respects a human-authored bump on the branch (no double-bump).
- [x] Missing manifest produces a clear error.
- [x] Unknown flag prints usage and exits non-zero.

Acceptance criteria mapping:

- *Version auto-bump happens at most once per PR unless a source change
  requires a new bump* — handled by `--run`'s base-vs-branch comparison
  plus re-check inside the `bump` job.
- *Re-run CI does not create duplicate bump commits* — covered by tests 7
  and 8 above.
- *Job ordering/dependencies with other checks are explicit* — the `bump`
  job declares `needs: guard` and the workflow file comments how other PR
  checks can opt in later.
