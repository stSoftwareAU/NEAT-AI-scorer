## Summary

Added a standalone Cargo Quality (fmt + clippy) GitHub Actions workflow at
`.github/workflows/cargo-quality.yml` per the workflow-sync template, with
the same hardening pattern (least-privilege permissions, pinned action
versions, NEAT-AI-core path-dependency checkout) used by the rest of the
CI suite. Adds a matching validator script and BATS tests so the workflow
cannot silently regress. Closes #66.

`ci.yml` already runs fmt and clippy, but only for PRs targeting `Develop`.
This new workflow fires on PRs against **any** branch
(`branches: ["*"]`), so feature branches and stacked PRs targeting
non-Develop bases get the same fmt + clippy gate without spinning up the
full CI graph.

The Codecov upload from the suggested template was deliberately omitted —
the repo has no `CODECOV_TOKEN` configured and adding
`codecov/codecov-action` would also require a new entry in
`scripts/check-workflow-action-versions.sh`. That should be a separate
follow-up rather than smuggled in under the fmt + clippy banner.

## Evidence

Backend / CI-only change — no UI to screenshot.

- `bats tests/scripts/cargo_quality_workflow.bats` — 11/11 pass.
- `./quality.sh < /dev/null` — full local gate passes (shellcheck,
  workflow validators including the new one, codespell, bats, cargo-deny,
  fmt, clippy, check, build, test, doc with `-D warnings`, release).
- `scripts/check-workflow-action-versions.sh` — every `uses:` reference in
  the new workflow satisfies the Node 24 policy (no new actions
  introduced).
- `scripts/check-workflow-paths.sh` — the new workflow uses the canonical
  `path: NEAT-AI-core` checkout strategy.

```mermaid
flowchart LR
    PR[PR opened against ANY branch] --> CQ[cargo-quality.yml]
    CQ --> Checkout[actions/checkout@v5]
    Checkout --> Sibling[Checkout NEAT-AI-core sibling]
    Sibling --> Toolchain[dtolnay/rust-toolchain<br/>+ rustfmt, clippy]
    Toolchain --> Fmt[cargo fmt --all -- --check]
    Fmt --> Clippy[cargo clippy --all-targets<br/>--all-features -- -D warnings]
```

## Test Plan

- [x] Added `tests/scripts/cargo_quality_workflow.bats` covering: canonical
      pass, missing `pull_request` trigger, missing permissions block,
      unpinned `actions/checkout`, missing `dtolnay/rust-toolchain`,
      missing `rustfmt`/`clippy` components, missing `cargo fmt --check`,
      missing `cargo clippy -D warnings`, missing workflow file, unknown
      flag, and end-to-end against the real `cargo-quality.yml`.
- [x] Added `scripts/check-cargo-quality-workflow.sh` — invoked from
      `quality.sh`, mirrors the structure of `check-cargo-audit-workflow.sh`.
- [x] Updated `quality.sh` to invoke the new validator alongside the
      existing workflow checks.
- [x] Updated `README.md` "Pull request automation" section to describe
      the new workflow.
- [x] Confirmed `./quality.sh < /dev/null` passes end-to-end.
