//! End-to-end smoke test for the `rust_scorer` binary.
//!
//! Runs the compiled binary against a tiny checked-in fixture (`tests/fixtures/`)
//! consisting of an identity creature and a 32-byte packed `f32` data file.
//!
//! Purpose: catch **API drift between `rust_scorer` and the path-dependency
//! `neat-core`** at PR time, instead of only when GRQ training fails downstream.
//! If the scorer fails to compile against the sibling `neat-core`, this test
//! never even runs (Cargo would fail earlier). If the scorer compiles but the
//! CLI contract / JSON output changes shape, this test fails loudly.
//!
//! Issue stSoftwareAU/NEAT-AI-scorer#11.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve `tests/fixtures/<name>` relative to this crate.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Resolve a directory containing `<name>` for tests that need a data directory
/// (the scorer takes a directory of `.bin` files, not a single file). Copies
/// the fixture into a fresh temporary directory so concurrent test runs don't
/// trip over each other.
fn fixture_data_dir(bin_name: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let src = fixture(bin_name);
    let dst = tmp.path().join(bin_name);
    std::fs::copy(&src, &dst)
        .unwrap_or_else(|e| panic!("copy fixture {} -> {}: {e}", src.display(), dst.display()));
    tmp
}

/// Run the `rust_scorer` binary against the fixture and assert the JSON output
/// matches the expected zero-error identity result.
#[test]
fn scorer_binary_runs_against_identity_fixture() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");

    let output = Command::new(bin)
        .arg(&creature)
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer binary");

    assert!(
        output.status.success(),
        "rust_scorer exited with status {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout is utf-8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON: {e}\n{stdout}"));

    // Identity network: every record contributes zero squared error.
    let error = parsed
        .get("error")
        .and_then(|v| v.as_f64())
        .expect("missing `error` in scorer JSON");
    assert!(error.abs() < 1e-6, "expected near-zero error, got {error}");

    // Score should be near 1.0 (perfect minus a tiny version penalty band).
    let score = parsed
        .get("score")
        .and_then(|v| v.as_f64())
        .expect("missing `score` in scorer JSON");
    assert!(
        score > 0.99 && score <= 1.0,
        "expected score close to 1.0, got {score}",
    );

    // Record count must match the fixture (4 records of 8 bytes each = 32 bytes).
    let record_count = parsed
        .get("recordCount")
        .and_then(|v| v.as_u64())
        .expect("missing `recordCount` in scorer JSON");
    assert_eq!(record_count, 4, "expected 4 records, got {record_count}");

    // Forward-only path was selected by the fixture creature.
    let forward_only = parsed
        .get("forwardOnly")
        .and_then(|v| v.as_bool())
        .expect("missing `forwardOnly` in scorer JSON");
    assert!(forward_only, "fixture sets forwardOnly: true");
}

/// Sanity check: missing creature file produces a non-zero exit and a clear error.
/// Also serves as a regression for the CLI contract (positional args).
#[test]
fn scorer_binary_fails_when_creature_missing() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let data_dir = fixture_data_dir("identity_data.bin");

    let output = Command::new(bin)
        .arg("/definitely/not/a/real/path/creature.json")
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer binary");

    assert!(
        !output.status.success(),
        "expected non-zero exit when creature file missing",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Failed to read creature file"),
        "expected diagnostic about missing creature file, got: {stderr}",
    );
}
