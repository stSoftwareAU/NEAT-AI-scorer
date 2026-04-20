## Summary

Documents the CLI's MSE-only cost-function scope as an intentional decision
rather than an accidental omission, following **Option 1** from issue #9. Adds
a new "Why MSE-only?" section to `README.md` covering:

- the fused fast path calls `neat_core::loss::mse_sum_batch_packed` directly
  and the non-fused path uses `cost::mse_mean_record` to match the TypeScript
  `MSE.calculate()` mean;
- today's caller (NEAT-AI `Develop`) never requests a non-MSE score, and
  `GROWTH_COST` / the fitness formula are defined against MSE;
- the sibling `neat-core` crate still exposes the full set of fused batch
  variants (`mae`, `cross_entropy`, `mape`, `msle`, `hinge` — all
  `_sum_batch_packed`), so re-adding a `--cost` dispatch would be CLI wiring
  plus tests, no new math, if a downstream caller ever needs it.

A short pointer in the `cost.rs` module docstring directs future contributors
to the README rationale and the `neat_core::loss` entry points.

Closes #9.

## Evidence

Documentation-only change — no UI, no behaviour change, no new CLI surface.

- `README.md` now contains a dedicated "Why MSE-only?" section.
- `rust_scorer/src/cost.rs` module doc comment references that section and the
  fused-loss entry points in `neat_core::loss`.
- No source logic, no public API, and no test expectations were modified.

## Test Plan

No new tests — the change is purely documentation.

- Existing `cost::tests::{test_mse_perfect_prediction, test_mse_known_value}`
  continue to cover `mse_mean_record` behaviour.
- Existing end-to-end CLI tests in `rust_scorer/src/main.rs` continue to cover
  the MSE scoring pipeline.

## Pre-existing issues (out of scope)

`./quality.sh` does not currently pass on this branch's base, because the
sibling `NEAT-AI-core` checkout has drifted ahead of what `rust_scorer`
expects (missing `for_each_read_chunk_with_mode`, `io_backend_label`, and
`training_read_tuning_from_env` in `neat_core::training_bin_stream`, plus a
`Clone` change on `CompiledNetwork`). Confirmed reproducible on
`main @ 9c295b3` with no staged changes. Fixing the integration drift is
unrelated to this docs-only issue and is therefore left out of scope.

## Acceptance checklist

- [x] Decision recorded in `README.md` so the MSE-only scope is intentional
      rather than implicit (Option 1).
- [x] `cost.rs` module docs point to the README rationale and to the fused
      `neat_core::loss::*_sum_batch_packed` entry points for any future
      Option-2 re-wiring.
