## Summary

Added the `repository` and `readme` metadata fields to `rust_scorer/Cargo.toml` so the crate complies with Rust API Guidelines C-METADATA. Without `repository`, `cargo publish` warns on every dry-run, SBOM and dependency-graph tools cannot resolve the crate back to GitHub, and `cargo doc` / IDE tooltips render an empty "Repository" link. The workspace pins `neat-core` via a sibling `path:` dep, so the upstream URL is the only canonical pointer to where this code came from. Closes #117.

## Evidence

This is a Cargo metadata-only change with no UI or performance impact. Verified via:

- `bats tests/scripts/cargo_metadata.bats` — 4/4 tests pass (file-level check plus `cargo metadata --no-deps` JSON inspection).
- `./quality.sh` — full local gate (shellcheck, cargo-deny, fmt, clippy, check, build, test, rustdoc `-D warnings`, release build) passes cleanly.

The resulting `[package]` block:

```toml
[package]
name = "rust_scorer"
version = "0.5.28"
edition = "2024"
license = "Apache-2.0"
description = "Native CLI binary for scoring NEAT-AI creatures against training data."
repository = "https://github.com/stSoftwareAU/NEAT-AI-scorer"
readme = "../README.md"
```

## Test Plan

- Added `tests/scripts/cargo_metadata.bats` with four cases:
  - `repository` line is present in `rust_scorer/Cargo.toml` and points at the canonical GitHub URL.
  - `readme` line is present.
  - The path named by `readme` resolves to an existing file relative to the crate root.
  - `cargo metadata --no-deps --format-version 1` reports the `repository` URL in the `rust_scorer` package JSON (catches the case where the field is set but malformed).
