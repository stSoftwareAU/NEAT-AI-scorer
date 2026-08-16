# PR summary — Issue #568

## Problem

Fleet guidance (VibeCoding#4159): **dev builds compile as fast as possible;
release builds produce the most optimised artefact possible.** This repo had
no `[profile.dev]`, used `lto = true` with `codegen-units = 1` only under
`[profile.release.package.rust_scorer]`, and had no `.cargo/config.toml` for
same-host `-C target-cpu=native`.

## Fix

1. `[profile.dev] debug = "line-tables-only"` in the workspace root
   `Cargo.toml` (panic file:line stays; full DWARF dropped).
2. Workspace-wide `[profile.release]`: `opt-level = 3`, `lto = "fat"`,
   `codegen-units = 1` — drop the per-package CGU scoping (and the matching
   `pgo` package override, since `pgo` inherits `release`).
3. Add `.cargo/config.toml` with
   `[target.'cfg(not(target_arch = "wasm32"))'] rustflags = ["-C", "target-cpu=native"]`,
   and un-ignore `.cargo/` in `.gitignore` so the file is tracked.
4. Document the profiles + `RUSTFLAGS`-replaces-config caveat in README /
   AGENTS; refresh the PGO section wording.

## Acceptance note

Touch-rebuild of `rust_scorer` after a warm `cargo build -p rust_scorer`
(Apple host, this PR): **before** (full `debuginfo`) Finished in **4.30 s**;
**after** (`line-tables-only`) Finished in **2.51 s** (~42 % faster).

PGO (`scripts/build-pgo.sh`) stays a separate opt-in — out of scope.

## Cross-reference

stSoftwareAU/VibeCoding#4159
