## Summary
Unblocks the `rust_scorer` build against the current `NEAT-AI-core` `Develop` branch (post commit `62a5c92`) and adds a pre-flight binary smoke test that catches API drift between `rust_scorer` and the `neat-core` path dependency at PR time — rather than only when the production trainer's `option=learn` fails downstream. Closes #11.

Two upstream breakages resolved in the scorer:

1. **`neat_core::mse_mean_record` signature changed** (NEAT-AI-core#15, merged). The new signature is `mse_mean_record(&mut CompiledNetwork, &[f32], usize, usize) -> f64` — it activates the network itself and consumes packed records. The scorer's recurrent fallback in `main.rs` was calling the old 2-arg shape. Since that loop already has per-record `inputs` / `outputs` / activation in scope, the cheapest fix is to compute the per-record MSE inline (`mean over outputs of (target - output)²`) and drop the import. This keeps the forward-only and recurrent paths numerically equivalent to `mse_mean_record` / `mse_sum_batch_packed` on any fixture where they see the same activations.

2. **`CompiledNetwork` lacks `Clone`** (NEAT-AI-core#11, still open). Rather than block on the upstream `#[derive(Clone)]`, `accumulate_mse_sum_forward_only_fused` now takes the source `&CreatureExport` and (re)compiles `worker_count` independent networks via `neat_core::creature::compile_creature` when multi-threaded activation is enabled. Each worker still owns its own activation/hint/trace buffers (same observable behaviour as a `Clone`), the scorer is self-sufficient against `neat-core` `Develop`, and the change is trivially swappable for `network.clone()` once NEAT-AI-core#11 lands.

Packaging + gate:

- Bumped `rust_scorer` to `0.1.4` so a fixed build can be published.
- Added `rust_scorer/tests/scorer_smoke.rs`, an end-to-end binary smoke test that runs the compiled `rust_scorer` against a checked-in 4-record identity fixture (`rust_scorer/tests/fixtures/identity_creature.json` + `identity_data.bin`, 32 bytes). It asserts the fused-stream path returns near-zero error, score ≈ 1.0, correct `recordCount`, and exits non-zero with a readable diagnostic when the creature file is missing. If the binary fails to compile against the sibling `neat-core` this test never runs — Cargo fails earlier — so the existing CI `cargo build --workspace` + `cargo test --workspace --all-features` gates already catch the exact "E0432/E0599 API drift" failure mode described in the issue.

CI gate verification:

- `.github/workflows/ci.yml` already runs `cargo build --workspace` and `cargo test --workspace --all-features` on every PR to `Develop` (lines 85–97). Both build and test the scorer. A dedicated `cargo build --release -p rust_scorer` step would be strictly redundant for catching API drift, but is a sensible belt-and-braces addition — I did not touch `ci.yml` in this PR because the automation worker does not hold the `workflow` OAuth scope needed to push workflow changes. The workflow change can be applied by a human in a follow-up.

## Evidence
Backend / CLI change — no UI to screenshot. Evidence is the passing quality gate and the new smoke test output.

Quality gate (full `./quality.sh`):

```text
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/scorer_smoke.rs (target/debug/deps/scorer_smoke-<hash>)

running 2 tests
test scorer_binary_fails_when_creature_missing ... ok
test scorer_binary_runs_against_identity_fixture ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.40s
…
🏗️ Building release...
    Finished `release` profile [optimized] target(s) in 15.02s
✅ All quality checks passed!
```

Build regression — before fix:

```text
error[E0432]: unresolved import `neat_core::mse_mean_record`
error[E0599]: no method named `clone` found for mutable reference `&mut CompiledNetwork`
```

After fix: `cargo build --release -p rust_scorer` → `Finished release profile [optimized] target(s)`.

## Test Plan
- `rust_scorer/tests/scorer_smoke.rs` — new. Two integration tests exercising the compiled binary:
  - `scorer_binary_runs_against_identity_fixture` — happy path, fused forward-only stream, asserts `error < 1e-6`, `0.99 < score ≤ 1.0`, `recordCount == 4`, `forwardOnly == true`.
  - `scorer_binary_fails_when_creature_missing` — error path, asserts non-zero exit and stderr diagnostic.
- `rust_scorer/tests/fixtures/identity_creature.json` — new. 1-input / 1-output identity creature with `forwardOnly: true`, `semanticVersion: "4.0.0"`.
- `rust_scorer/tests/fixtures/identity_data.bin` — new. 32 bytes = 4 records × (1 input + 1 target) × 4-byte `f32`, all matching pairs so MSE is 0.
- Existing 23 unit tests in `rust_scorer/src/main.rs` and submodules still pass (including `test_identity_network_zero_error`, `test_version_penalty_in_score`, etc.).
