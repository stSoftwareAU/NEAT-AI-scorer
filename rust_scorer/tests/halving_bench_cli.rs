//! Behavioural contract for the opt-in experiment command.
use std::process::Command;

use rust_scorer::fixture_json::{creature_envelope, neuron_json, synapse_json};
use tempfile::TempDir;

fn fixture() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("candidates")).unwrap();
    std::fs::create_dir(dir.path().join("data")).unwrap();
    for i in 0..12 {
        let json = creature_envelope(1, 1,
            &[neuron_json("output", "output-0", i as f64 / 10.0, "IDENTITY")],
            &[synapse_json("input-0", "output-0", 1.0)]);
        std::fs::write(dir.path().join(format!("candidates/c{i:02}.json")), json).unwrap();
    }
    let bytes: Vec<u8> = (0..200).flat_map(|i| {
        let x = i as f32 / 200.0;
        [x, x + 0.3].into_iter().flat_map(f32::to_le_bytes)
    }).collect();
    std::fs::write(dir.path().join("data/a.bin"), bytes).unwrap();
    dir
}

fn command(dir: &TempDir) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_halving_bench"));
    c.arg(dir.path().join("candidates")).arg(dir.path().join("data"));
    c.env("NEAT_SCORER_ACTIVATION_THREADS", "1");
    c.stdin(std::process::Stdio::null());
    c
}

#[test]
fn benchmark_reports_four_controls_and_authoritative_rejection_audits() {
    let dir = fixture();
    let before = std::fs::read(dir.path().join("candidates/c03.json")).unwrap();
    let out = command(&dir).arg("--cost").arg("RMSE").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema"], "neat-halving-observation/1");
    assert_eq!(v["records"], 200);
    assert_eq!(v["authoritativeRecordEvaluations"], 2400);
    let arms = v["arms"].as_array().unwrap();
    assert_eq!(arms.len(), 4);
    for arm in arms {
        assert_eq!(arm["audit"]["fullWinner"], 3);
        assert_eq!(arm["audit"]["regret"], 0.0);
        assert_eq!(arm["outcome"]["selected"], 3);
        assert!(arm["maxRelativeDeviation"].as_f64().unwrap() <= 1e-6);
        assert_eq!(arm["rejected"].as_array().unwrap().len(),
            arm["outcome"]["eliminatedAt"].as_array().unwrap().iter().filter(|v| !v.is_null()).count());
    }
    assert_eq!(std::fs::read(dir.path().join("candidates/c03.json")).unwrap(), before);
}

#[test]
fn benchmark_refuses_limits_and_malformed_data_loudly() {
    let dir = fixture();
    let out = command(&dir).arg("--max-corpus-bytes").arg("16").output().unwrap();
    assert!(!out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "failed");
    assert!(v["error"].as_str().unwrap().contains("corpus byte limit"));
    // With the limit lifted, this input reaches scoring (the positive control).
    assert!(command(&dir).output().unwrap().status.success());
    std::fs::write(dir.path().join("data/a.bin"), [0_u8; 9]).unwrap();
    let out = command(&dir).output().unwrap();
    assert!(!out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["error"].as_str().unwrap().contains("record"));
}
