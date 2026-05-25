//! Smoke test for the `cost_scan_bench` binary (Issues #124, #134).
//!
//! Drives the binary end-to-end against a synthetic creature + corpus and
//! asserts the JSON output carries one row per supported `CostKind`.
//! Issue #134 unblocked `CATEGORICAL_ERROR` (dispatch landed via
//! `NEAT-AI-core#88`), so the sweep now covers all seven built-in costs
//! and the `skipped` array is expected to be empty. The bench bin itself
//! produces the per-cost numbers that Issue #124 records on the issue
//! thread; this test guards the CLI contract so the script that consumes
//! the JSON keeps working.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

/// Minimal forward-only identity creature: 1 input → 1 output, IDENTITY
/// squash, weight 1.0. Small enough that the fused path runs in
/// milliseconds even on the largest corpus this smoke test creates.
const IDENTITY_CREATURE_JSON: &str = r#"{
    "input": 1,
    "output": 1,
    "forwardOnly": true,
    "semanticVersion": "4.0.0",
    "neurons": [
        {"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}
    ],
    "synapses": [
        {"fromUUID":"input-0","toUUID":"output-0","weight":1.0}
    ]
}"#;

/// Write a tiny `.bin` corpus: 64 records of [input, target] f32 pairs.
fn write_tiny_corpus(data_dir: &std::path::Path) {
    fs::create_dir_all(data_dir).unwrap();
    let mut bytes: Vec<u8> = Vec::new();
    for i in 0_u32..64 {
        let x = (i as f32) * 0.01;
        let t = x + 0.1;
        bytes.extend_from_slice(&x.to_le_bytes());
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    fs::write(data_dir.join("0.bin"), &bytes).unwrap();
}

fn bench_bin() -> PathBuf {
    // Cargo sets `CARGO_BIN_EXE_<name>` for any bin target the integration
    // test depends on (the bin is declared in `Cargo.toml`).
    PathBuf::from(env!("CARGO_BIN_EXE_cost_scan_bench"))
}

#[test]
fn cost_scan_bench_emits_one_row_per_supported_cost() {
    let tmp = TempDir::new().unwrap();
    let creature = tmp.path().join("creature.json");
    fs::write(&creature, IDENTITY_CREATURE_JSON).unwrap();
    let data_dir = tmp.path().join("data");
    write_tiny_corpus(&data_dir);

    let output = Command::new(bench_bin())
        .arg(&creature)
        .arg(&data_dir)
        .arg("--runs")
        .arg("2")
        .output()
        .expect("run cost_scan_bench");

    assert!(
        output.status.success(),
        "bench exited non-zero: status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("invalid JSON from bench: {e}\nstdout:\n{stdout}"));

    let rows = json["rows"].as_array().expect("rows must be a JSON array");

    // Issue #134: every built-in cost should now produce a measured row.
    let want = [
        "MSE",
        "MAE",
        "MAPE",
        "MSLE",
        "HINGE",
        "CROSS_ENTROPY",
        "CATEGORICAL_ERROR",
    ];
    assert_eq!(
        rows.len(),
        want.len(),
        "expected {} rows, got {}: {}",
        want.len(),
        rows.len(),
        stdout
    );
    for name in want {
        let row = rows
            .iter()
            .find(|r| r["cost"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("missing row for cost {name}\nstdout:\n{stdout}"));
        // Each row must carry a finite median time and a throughput estimate.
        let median = row["medianMs"]
            .as_f64()
            .unwrap_or_else(|| panic!("medianMs missing for {name}: {row}"));
        assert!(
            median.is_finite() && median >= 0.0,
            "medianMs not finite for {name}: {median}"
        );
        let rps = row["recordsPerSec"]
            .as_f64()
            .unwrap_or_else(|| panic!("recordsPerSec missing for {name}: {row}"));
        assert!(
            rps.is_finite() && rps > 0.0,
            "recordsPerSec must be positive for {name}: {rps}"
        );
    }

    // The `skipped` array remains in the JSON contract for
    // forwards-compatibility but must be empty now that every built-in
    // cost dispatches.
    let skipped = json["skipped"]
        .as_array()
        .expect("skipped must be an array");
    assert!(
        skipped.is_empty(),
        "no costs should be skipped after #134, got: {skipped:?}"
    );
}

#[test]
fn cost_scan_bench_rejects_missing_data_dir() {
    let tmp = TempDir::new().unwrap();
    let creature = tmp.path().join("creature.json");
    fs::write(&creature, IDENTITY_CREATURE_JSON).unwrap();
    let missing = tmp.path().join("no-such-dir");

    let output = Command::new(bench_bin())
        .arg(&creature)
        .arg(&missing)
        .arg("--runs")
        .arg("1")
        .output()
        .expect("run cost_scan_bench");

    assert!(
        !output.status.success(),
        "bench must exit non-zero when data_dir is missing"
    );
}
