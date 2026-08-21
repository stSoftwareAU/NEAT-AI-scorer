//! Smoke test for the `if_tree_batch_bench` binary — Issue #574.
//!
//! Guards the JSON contract the Forests batching bench emits: one report
//! carrying **candidates/second** and **records/second** for an `IF`-heavy
//! candidate batch, and a fail-loud exit when the run cannot be honoured.

use std::path::PathBuf;
use std::process::Command;

fn bench_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_if_tree_batch_bench"))
}

#[test]
fn bench_reports_candidate_and_record_rates() {
    let output = Command::new(bench_bin())
        .args([
            "--candidates",
            "6",
            "--records",
            "512",
            "--depth",
            "2",
            "--runs",
            "1",
            "--graft-every",
            "3",
            "--gpu",
            "off",
        ])
        .output()
        .expect("bench runs");
    assert!(
        output.status.success(),
        "bench failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("bench emits a JSON object");
    assert_eq!(json["candidates"], 6);
    assert_eq!(json["records"], 512);
    assert_eq!(json["graftedCandidates"], 2);
    assert_eq!(json["gpuBackend"], "cpu-fallback");
    for rate in [
        "candidatesPerSec",
        "recordsPerSec",
        "candidateRecordEvaluationsPerSec",
    ] {
        let v = json[rate]
            .as_f64()
            .unwrap_or_else(|| panic!("{rate} is a number"));
        assert!(v > 0.0, "{rate} must be positive, got {v}");
    }
    // The bench ranks the batch, so the winning candidate is named and its loss
    // is a real (finite) number.
    assert!(
        json["bestCandidate"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert!(json["bestError"].as_f64().is_some_and(f64::is_finite));
}

#[test]
fn bench_fails_loudly_on_an_empty_batch() {
    let output = Command::new(bench_bin())
        .args(["--candidates", "0", "--records", "512", "--gpu", "off"])
        .output()
        .expect("bench runs");
    assert!(
        !output.status.success(),
        "an empty batch must exit non-zero rather than report an empty result as success"
    );
}
