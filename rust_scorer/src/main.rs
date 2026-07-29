//! Thin shim for the `rust_scorer` binary (Issue #475).
//!
//! All CLI logic lives in [`rust_scorer::cli`]. The bin target links the
//! library instead of declaring its own `mod` tree, so every module is
//! compiled once and `dead_code` stays armed across the crate.

fn main() {
    rust_scorer::cli::main();
}
