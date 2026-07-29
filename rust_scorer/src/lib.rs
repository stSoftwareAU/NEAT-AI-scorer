//! Library surface for `rust_scorer` so external targets (Criterion benches,
//! integration tests) can call the same hot paths the CLI uses.
//!
//! The CLI binary in `src/main.rs` continues to declare its own `mod ...;`
//! tree — keeping `main.rs` self-contained avoids touching the stable
//! positional CLI contract while still letting the `benches/scoring.rs`
//! Criterion harness import the scoring modules through this crate root.
//!
//! Issue #36 — Criterion benchmark infrastructure.

// Crate-level rustc lint hardening (Issue #274). `missing_docs` is scoped to
// the library surface here rather than in `[workspace.lints.rust]` so the
// doc-noisy binary targets are not forced to document every internal item.
// Enforces doc discipline on the public API this crate exposes to benches and
// integration tests.
#![warn(missing_docs)]

pub mod corpus_guard;
pub mod cost;
pub mod env_tuning;
pub mod gpu;
pub mod multi_score;
pub mod prod_fixture;
pub mod read_tuning;
pub mod sampling;
pub mod scoring;
pub mod shallow_fixture;
pub mod stream_io;
pub mod stream_score;
