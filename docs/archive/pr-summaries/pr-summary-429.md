## Summary

Removed the unused public constant `PRODUCTION_CORPUS_BYTES` from
`rust_scorer/src/prod_fixture.rs`. A whole-repo token search across every `.rs`,
`.md`, `.sh`, `.toml`, `.yml` and `.wgsl` file found exactly one occurrence — the
declaration itself. Because `prod_fixture` exists only in the library target
(`src/main.rs` does not declare it in the binary's module tree), the compiler's
`dead_code` lint never analysed its `pub` items, so the unreferenced export sat
undetected. Closes #429.

The constant recorded production-corpus provenance
(`training_data_size_bytes = 20 845 703 976`, ≈ 19.4 GiB). That figure is
**already** documented in `docs/performance-baseline.md` (Corpus sizing section),
so deleting the code loses no information — no doc move was required.

Scope: only `PRODUCTION_CORPUS_BYTES` is removed here. The adjacent
`PRODUCTION_CORPUS_FILES` constant is tracked by sibling finding
BP-ff5170aef37b and is left untouched.

## Evidence

Backend/CLI change only — no web interface to screenshot.

Verification:

- `grep -rn "PRODUCTION_CORPUS_BYTES" --include="*.rs" ...` now returns no
  matches (previously matched only the declaration).
- `cargo check --workspace --all-targets` — clean.
- `./quality.sh` — `✅ All quality checks passed!` (fmt, cargo-deny, clippy,
  check, build, test, rustdoc, release build).

```mermaid
flowchart LR
    A["prod_fixture.rs<br/>PRODUCTION_CORPUS_BYTES<br/>(unused pub const)"] -->|deleted| B["figure retained in<br/>docs/performance-baseline.md"]
```

## Test Plan

This is a dead-code removal: an unreferenced constant with no behaviour, so no
new unit test applies (there is nothing to assert). The existing `prod_fixture`
test module continues to pass unchanged. Correctness is confirmed by the full
`./quality.sh` gate — in particular the workspace `unused = "deny"` lint and the
complete `cargo test` suite — remaining green after the deletion.
