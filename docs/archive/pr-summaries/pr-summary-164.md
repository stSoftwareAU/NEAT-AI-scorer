## Summary

Added a `// SAFETY:` comment to the previously bare `unsafe` block in
`rust_scorer/src/bin/float_scan_bench.rs` (in `sum_f32_le_bytes`). The block
performs raw pointer arithmetic (`p.add(i * 4)`) and an unaligned 4-byte read;
the comment documents the invariants that make it sound — `data.len()` is a
multiple of 4 (asserted above) and `i < n == data.len() / 4` — mirroring the
sibling SAFETY notes already present in `multi_score.rs` and `stream_score.rs`.
This restores the crate-wide convention that every `unsafe` block carries a
named-invariant comment. No behaviour changed. Closes #164

## Evidence

Backend/CLI-only change — no web interface to screenshot. Verified via the
crate's quality gate (`./quality.sh`): `fmt --check`, clippy (`-D warnings`),
`check`, `build`, `test`, rustdoc and the release build all pass cleanly.

Added unit tests exercise the `unsafe` pointer-arithmetic loop directly,
demonstrating the SAFETY invariants hold for empty, single, and multi-element
inputs:

```text
running 3 tests
test tests::sums_empty_slice_to_zero ... ok
test tests::sums_single_value ... ok
test tests::sums_multiple_values_exercising_pointer_arithmetic ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## Test Plan

- Added `tests::sums_empty_slice_to_zero` — `sum_f32_le_bytes(&[])` returns `0.0` (edge case).
- Added `tests::sums_single_value` — single `f32` round-trips correctly (happy path).
- Added `tests::sums_multiple_values_exercising_pointer_arithmetic` — multiple
  elements force the `unsafe { p.add(i * 4) ... }` loop across every in-bounds
  offset and the sum matches the `f64` reference (exercises the documented block).
