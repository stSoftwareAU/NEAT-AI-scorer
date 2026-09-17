# neat-core semver signalling (Issue #251)

Tracking record for **Issue #251** — *"neat-core: signal breaking changes via a
semver (major-equivalent) bump + git tags/releases"* — part of the
release-process redesign epic **#248**.

## Context

At the time of writing, `rust_scorer` depended on **`neat-core`** as a **path
dependency** at head (`../../NEAT-AI-core/neat-core`); it now pins a release
tag (Issue #630 — see the README
[neat-core release pin](../../README.md#neat-core-release-pin-issue-630)). When
neat-core
[#177](https://github.com/stSoftwareAU/NEAT-AI-core/issues/177) narrowed
`SynapseData::from_index` from `u32` to `u16` — a **breaking** type change — it
shipped with **no signal**:

- neat-core had **no semver tags** (only `wasm-bundle-<sha>` artifacts), and
- its CI auto-bumped only the **patch** (`0.1.43 → 0.1.46`).

So the scorer, tracking the path dep at head, **broke silently**.

## Resolution (lands in NEAT-AI-core)

The fix lives in the neat-core repository — companion PR
**[stSoftwareAU/NEAT-AI-core#190](https://github.com/stSoftwareAU/NEAT-AI-core/pull/190)**:

- **Policy** (`RELEASING.md`): a **breaking** change is a **major-equivalent**
  bump — pre-1.0 that is the **minor** (`0.1.x → 0.2.0`); non-breaking changes
  bump the **patch**.
- **Enforcement**: a `version-gate` CI job fails any PR whose breaking change
  ships on a patch-only bump; the `version-increment` job bumps the minor when a
  break is signalled (the `breaking-change` label or a Conventional Commit
  `type!:` / `BREAKING CHANGE:` marker).
- **Discoverability**: a `release` workflow cuts a **`v<version>`** git tag +
  GitHub release on every version bump, **decoupled** from the existing
  `wasm-bundle-<sha>` artifacts.
- **Retro decision**: the #177 break is retro-bumped `0.1.46 → 0.2.0` (the `u16`
  narrowing is **not** reverted).

## Scorer-side implication

This unblocks the **scorer CI gate** (sibling sub-issue of #248): that gate can
key off neat-core's now-signalled semver — comparing the pinned/observed
neat-core version and reacting to a major-equivalent (minor pre-1.0) bump as a
breaking change — instead of discovering breaks at compile/runtime.

No scorer code change was required for #251 itself: the scorer consumed
neat-core via a **path** dependency, so there was no version pin to bump here.
Issue #630 later replaced that path dependency with a release-tag pin, which is
exactly the signal #251 asked neat-core to publish. Per the
release-gating policy, cutting the `v0.2.0` release is a **human-gated** step
performed by neat-core's `release` workflow on merge of PR #190.
