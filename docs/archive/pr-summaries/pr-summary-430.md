## Summary

Removed the unused public constant `PRODUCTION_CORPUS_FILES` from
`rust_scorer/src/prod_fixture.rs`. A whole-repo token search across every `.rs`,
`.md`, `.sh`, `.toml`, `.yml` and `.wgsl` file found only the declaration itself
(plus a passing mention in the #429 PR summary). Because `prod_fixture` exists
only in the library target (`src/main.rs` does not declare it in the binary's
module tree), the compiler's `dead_code` lint never analysed its `pub` items, so
the unreferenced export sat undetected. Closes #430.

The constant recorded production-corpus provenance (`training_data_files = 520`
from GRQ-cluster `performance.csv`). That figure is **already** documented in
`docs/performance-baseline.md` (Corpus sizing section, line 692), so deleting the
code loses no information — no doc move was required. This mirrors the #429
removal of the adjacent `PRODUCTION_CORPUS_BYTES` constant.

## Evidence

Backend/CLI change only — no web interface to screenshot.

Verification:

- `grep -rn "PRODUCTION_CORPUS_FILES" --include="*.rs"` now returns no matches
  (previously matched only the declaration).
- `cargo check --workspace --all-targets` — clean.
- `./quality.sh` — `✅ All quality checks passed!` (shellcheck, cargo-deny,
  fmt --check, clippy, check, build, test, rustdoc, release build).

```mermaid
flowchart LR
    A["prod_fixture.rs<br/>PRODUCTION_CORPUS_FILES<br/>(unused pub const)"] -->|deleted| B["figure retained in<br/>docs/performance-baseline.md"]
```

## Test Plan

This is a dead-code removal: an unreferenced constant with no behaviour, so no
new unit test applies (there is nothing to assert). The existing `prod_fixture`
test module continues to pass unchanged. Correctness is confirmed by the full
`./quality.sh` gate — in particular the workspace `unused = "deny"` lint and the
complete `cargo test` suite — remaining green after the deletion.
