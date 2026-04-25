//! Library surface for `rust_scorer` so external targets (Criterion benches,
//! integration tests) can call the same hot paths the CLI uses.
//!
//! The CLI binary in `src/main.rs` continues to declare its own `mod ...;`
//! tree — keeping `main.rs` self-contained avoids touching the stable
//! positional CLI contract while still letting the `benches/scoring.rs`
//! Criterion harness import the scoring modules through this crate root.
//!
//! Issue #36 — Criterion benchmark infrastructure.

pub mod multi_score;
pub mod read_tuning;
pub mod scoring;
pub mod stream_score;
