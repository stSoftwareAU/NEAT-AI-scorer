# PR summary — Issue #542

## Problem

Workers repeatedly printed `Updating neat-core v0.8.12 -> v0.9.0` even when
`model_fetch` skipped the NEAT-AI-core git update. Cause: `NEAT-AI-scorer`'s
committed `Cargo.lock` lagged the sibling path dependency. Every
`ensure_neat_ai_native_scorer` `cargo build` rewrote the lock locally; the next
`model_fetch` hard-reset restored the stale lock.

PR automation already ran rustfmt (#19) and the guarded version bump (#20), but
never refreshed `Cargo.lock` against latest NEAT-AI-core `Develop`.

## Fix

1. Extend `.github/workflows/auto-format.yml` to run
   `cargo update -p neat-core` after `cargo fmt --all`, then commit/push when
   the tracked tree is dirty (same ACTIONS_PUSH / App-token pattern).
2. Teach `scripts/check-auto-format-workflow.sh` + BATS to require that step.
3. Acknowledge neat-core `0.9.0` in `neat-core.expected-version` (SOA hot-synapse
   API break is unused by scorer) and commit the matching `Cargo.lock` sync.

Deliberately does **not** auto-bump `neat-core.expected-version` on every PR —
the Issue #252 breaking-bump gate stays a human acknowledgement.

## Maintainer note (workflow YAML)

This PR edits `.github/workflows/auto-format.yml`. If the automation worker
cannot push workflow changes, a maintainer must merge (or apply) that YAML
diff. See CONTRIBUTING.md "Human escalation".
