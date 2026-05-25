## Summary

Harden the workflow-action policy so `actions/setup-node` cannot regress
to a Node 20 release. Fixes #137

The immediate `setup-node@v4` (Node 20) usage that triggered the
deprecation warning in Issue #137 was already removed in PR #135 (the
`markdown-lint.yml` step now pins `actions/setup-node@v6` — Node 24).
However, the validator in
`scripts/check-workflow-action-versions.sh` still allowed any
`actions/setup-node` >= v4, so a future bump back to v4 or v5 (both
Node 20) would have passed the gate. This PR raises the policy floor to
`required:6` — v6 is the first `actions/setup-node` release on Node 24
— and adds regression tests for v4 and v5.

## Evidence

Backend-only change to a workflow-policy shell script — no UI to
screenshot. Tests verify behaviour:

- `bats tests/scripts/workflow_action_versions.bats` — 17/17 pass,
  including the two new Issue #137 regression cases.
- `./quality.sh` was run end-to-end; the only failure is the
  pre-existing `cargo metadata` test, which requires a sibling
  `NEAT-AI-core` clone that is not present in the local auto-issue-work
  environment (see AGENTS.md). CI has the sibling clone via the
  workflow path strategy.

Policy change:

```diff
-    actions/setup-node)               echo "required:4" ;;
+    # actions/setup-node v4 and v5 still ship a Node 20 runtime; v6.0.0
+    # (2025-10) is the first Node 24 release. Pinning the floor at v6
+    # prevents the deprecation regression that triggered Issue #137.
+    actions/setup-node)               echo "required:6" ;;
```

## Test Plan

- Added `tests/scripts/workflow_action_versions.bats::fails when actions/setup-node version comment is older than v6 (Node 20 — Issue #137)` — fixture pins setup-node @v4 and asserts the validator exits non-zero with `requires v6 or newer`.
- Added `tests/scripts/workflow_action_versions.bats::fails when actions/setup-node is pinned to v5 (still Node 20 — Issue #137)` — same shape, pinning @v5, to lock the v5→Node 20 case explicitly.
- Extended the existing `write_compliant_workflow` fixture with an `actions/setup-node@<sha>  # v6` line so the happy-path test exercises the new policy too.
- Existing `real repository workflows satisfy the Node 24 compat policy` test continues to pass, confirming `.github/workflows/markdown-lint.yml` already complies.
