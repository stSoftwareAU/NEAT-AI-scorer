## Summary

Migrated `rust_scorer` to **wgpu 30** so the automated Dependabot run for the
wgpu 29 → 30 bump (#669) builds and goes green, matching NEAT-AI-Discovery,
which is already on wgpu 30.0.1. Closes #670.

wgpu 30 broke two call sites, which made the Dependabot PR's `Quality Checks`
and `Cargo Format and Clippy` jobs fail (`E0308` on `get_mapped_range`,
`E0063` missing `apply_limit_buckets`):

- `Buffer::get_mapped_range()` now returns `Result<BufferView, MapRangeError>`.
  The partials readback in `forward_mse_batched.rs` surfaces a failure through a
  new `mapped_range_result` helper as a recoverable `Err(String)`, beside the
  existing `map_readback_result`. It is not unwrapped.
- `RequestAdapterOptions` gained `apply_limit_buckets`. `select_adapter` sets it
  to `false` so the adapter keeps its real limits, which `HostResources::gpu`
  reads (`max_storage_buffer_binding_size`, workgroup limits).

`Cargo.lock` was moved with `cargo update -p wgpu` only. The changed packages
are the wgpu stack (wgpu, wgpu-core, wgpu-hal, wgpu-types, naga 29.0.4 →
30.0.1, `bit-set` 0.9 → 0.10) and their transitive adds and drops. The
`neat-core` pin is unchanged.

## Spec

### Intent and Rationale

- The issue's screenshot shows Dependabot PR #669 ("Bump wgpu from …") with a failing commit. Its CI log shows a compile failure from the wgpu 30 API change, not a flaky check.
- The family precedent is to migrate rather than pin back. NEAT-AI-Discovery's AGENTS.md records the same 29 → 30 break (Discovery Issue #1594) and its fix (`apply_limit_buckets: false`, fallible `get_mapped_range`).

### Essential Design Decisions

- `apply_limit_buckets: false`. Bucketing exists to resist fingerprinting for untrusted content (`wgpu-types-30.0.1/src/adapter.rs:56`). Bucketed limits would skew the #548 GPU-capability clamps.
- A failed `get_mapped_range` is an `Err(String)` on the same path as the #273/#583 readback errors, so `--gpu auto` still falls back to CPU and `--gpu on` fails loud.

### Undiscoverable Facts

- The device-lost panic payload that `gpu::device_loss` classifies is unchanged in wgpu 30.0.1. It comes from `handle_error_fatal` / `format_error` in `wgpu-30.0.1/src/backend/wgpu_core.rs:344-379`, called from `Device::poll` at `:1916-1924`, and `DeviceError::Lost` is still `"Parent device is lost"` (`wgpu-core-30.0.1/src/device/mod.rs:344`). Only the two comments that name the version changed.
- The "12 checks awaiting approval" and "A review is required" banners in the screenshot come from repository settings and rulesets. This diff does not change them, and no workflow file is touched.

## Evidence

Backend/CLI change only, with no UI surface.

- `./quality.sh < /dev/null` on the head: `✅ All quality checks passed!` (clippy `-D warnings`, the full test suite, rustdoc and the release build all ran against wgpu 30.0.1).
- The Dependabot PR #669 CI log (run 37184221761) failed with `error[E0308]: mismatched types … found &Result<BufferView, MapRangeError>` and `error[E0063]: missing field apply_limit_buckets`. Both are fixed here.

**Docs sweep** — grep: `wgpu.{0,3}29`, `29\.x`, `wgpu-29`, `get_mapped_range`, `apply_limit_buckets` over `README.md`, `AGENTS.md`, `CONTRIBUTING.md`, `docs/` (excluding `docs/archive/`); section: `README.md#device-loss-mid-run-issue-583`, read through and still accurate; updated: the doc comments in `rust_scorer/src/gpu/device_loss.rs` and `rust_scorer/tests/gpu_device_loss_fallback.rs`; `docs/performance-baseline.md:628` — still true because it records what the adapter reported in a past wgpu 29 measurement; `docs/performance-baseline.md:1168` — still true because it describes the toolchain of that recorded Host A run; `README.md:267` — still true because it quotes the historical #583 fleet panic, whose payload is unchanged in wgpu 30.

## Test Plan

- Added `rust_scorer/src/gpu/forward_mse_batched.rs::tests::mapped_range_result_ok_on_successful_map` and `::mapped_range_result_err_on_map_failure`. Both pass on the head.
- Existing GPU device-loss tests (`rust_scorer/tests/gpu_device_loss_fallback.rs`, `rust_scorer/src/gpu/device_loss.rs` tests) are unchanged apart from comments, and they pass.
- `cargo test --workspace`: 587 passed. `./quality.sh < /dev/null`: passed on the head.
- No assertion was removed from any existing test.
- The changed call site `forward_mse_batched.rs:852` (`mapped_range_result(slice.get_mapped_range())?`) is guarded by the compiler. Reverting it to the bare wgpu 29 form does not compile against wgpu 30 (the original `E0308`). The same applies to `apply_limit_buckets` in `gpu/mod.rs` (`E0063`).

**Branch outcomes:**

- `rust_scorer/src/gpu/forward_mse_batched.rs:1205`: `Ok` passes the value through. Reached by `mapped_range_result_ok_on_successful_map`.
- `rust_scorer/src/gpu/forward_mse_batched.rs:1205`: `Err` gives `"partials get_mapped_range failed: …"`. Reached by `mapped_range_result_err_on_map_failure`. Replacing the message with `String::new()` turned that test red (1 failed), and the code was then restored.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
