//! Library surface for `rust_scorer` so external targets (Criterion benches,
//! integration tests) can call the same hot paths the CLI uses.
//!
//! Since Issue #475 the CLI binary is a thin shim over [`cli::main`]: `src/main.rs`
//! links this library rather than declaring its own `mod ...;` tree. One copy
//! of every module means the shipped binary cannot drift from the code benches
//! and integration tests exercise, and `dead_code` is armed crate-wide instead
//! of being blanket-suppressed for a duplicate bin-side compilation.
//!
//! Issue #36 — Criterion benchmark infrastructure.

// Crate-level rustc lint hardening (Issue #274). `missing_docs` is scoped to
// the library surface here rather than in `[workspace.lints.rust]` so the
// doc-noisy binary targets are not forced to document every internal item.
// Enforces doc discipline on the public API this crate exposes to benches and
// integration tests.
#![warn(missing_docs)]

pub mod cli;
pub mod corpus_guard;
pub mod cost;
pub mod env_tuning;
pub mod fixture_json;
pub mod gpu;
pub mod multi_score;
pub mod prod_fixture;
pub mod read_tuning;
pub mod sampling;
pub mod scoring;
pub mod shallow_fixture;
pub mod stream_io;
pub mod stream_score;
