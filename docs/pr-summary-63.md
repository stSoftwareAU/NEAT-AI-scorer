## Summary

Adds the standalone Markdown Lint GitHub Actions workflow requested by workflow-sync tooling. The workflow installs `markdownlint-cli2` and runs it against the existing `.markdownlint-cli2.yaml` config on every pull request and on pushes to `main`/`master`. Closes #63.

To match the repo's existing convention (every workflow has a paired validator + bats suite + `quality.sh` hook), this PR also adds:

- `scripts/check-markdown-lint-workflow.sh` — validates the workflow's triggers, least-privilege permissions, action pins, and that `markdownlint-cli2` is both installed and invoked.
- `tests/scripts/markdown_lint_workflow.bats` — 11 bats tests exercising the validator with synthetic fixtures.
- A new `quality.sh` step that runs the validator alongside the other workflow gates.
- `actions/setup-node` is now in the Node 24 policy table (`required:4`) so the new workflow does not trip the unknown-action warning.

The optional Deno-based Mermaid validation block from the issue template is omitted — `worker/deno/mod.ts` does not exist in this Rust-only repo, so the conditional block would always skip.

## Evidence

CLI gate (no UI to screenshot):

```text
$ ./scripts/check-markdown-lint-workflow.sh
OK   .../markdown-lint.yml: triggers on pull_request
OK   .../markdown-lint.yml: permissions block grants only contents: read
OK   .../markdown-lint.yml: actions/checkout pinned to a numeric major
OK   .../markdown-lint.yml: actions/setup-node pinned to a numeric major
OK   .../markdown-lint.yml: markdownlint-cli2 install step present
OK   .../markdown-lint.yml: markdownlint-cli2 invoked

$ bats tests/scripts/markdown_lint_workflow.bats
1..11
ok 1 passes on the canonical fixture
ok 2 fails when the workflow is not triggered on pull_request
ok 3 fails when the permissions block is missing
ok 4 fails when actions/checkout is unpinned
ok 5 fails when actions/setup-node is unpinned
ok 6 fails when actions/setup-node step is missing
ok 7 fails when markdownlint-cli2 install step is missing
ok 8 fails when markdownlint-cli2 is not invoked
ok 9 reports an error when the workflow file does not exist
ok 10 unknown flag prints usage and exits non-zero
ok 11 real repository markdown-lint workflow satisfies every rule
```

Full bats suite (`bats tests/scripts/`) — 169 tests pass, none fail.

```mermaid
flowchart LR
    PR[Pull request opened] --> WF[markdown-lint.yml]
    WF --> CO[actions/checkout@v5]
    CO --> SN[actions/setup-node@v4]
    SN --> IN[npm install -g markdownlint-cli2]
    IN --> RUN[markdownlint-cli2]
    RUN --> RES{Lint clean?}
    RES -- yes --> PASS[ Job passes]
    RES -- no --> FAIL[ Job fails — block merge]
```

## Test Plan

- Added `tests/scripts/markdown_lint_workflow.bats` (11 cases) covering the canonical workflow plus targeted failure modes for each rule the validator enforces.
- Verified `scripts/check-workflow-action-versions.sh` still passes after adding `actions/setup-node` to the policy (`bats tests/scripts/workflow_action_versions.bats`).
- Ran the full `tests/scripts/` bats suite — 169 / 169 pass.
