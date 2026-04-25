use std::io::Write;
use std::path::Path;
use std::process::Command;

fn write_training_data(dir: &Path, records: &[(Vec<f32>, Vec<f32>)]) {
    let mut file = std::fs::File::create(dir.join("0.bin")).expect("create data file");
    for (inputs, outputs) in records {
        for &v in inputs.iter().chain(outputs.iter()) {
            file.write_all(&v.to_le_bytes()).expect("write f32");
        }
    }
}

fn minimal_creature(input: usize, output: usize, forward_only: bool) -> String {
    format!(
        r#"{{"input":{input},"output":{output},"forwardOnly":{forward_only},"neurons":[{{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}}],"synapses":[{{"fromUUID":"input-0","toUUID":"output-0","weight":1.0}}]}}"#
    )
}

#[test]
fn directory_mode_uses_filename_stems_as_keys() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let tmp = tempfile::tempdir().expect("create tempdir");
    let creatures_dir = tmp.path().join("creatures");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir(&data_dir).expect("create data dir");

    std::fs::write(
        creatures_dir.join("alpha-1.json"),
        minimal_creature(1, 1, true),
    )
    .expect("write creature a");
    std::fs::write(
        creatures_dir.join("beta_2.json"),
        minimal_creature(1, 1, true),
    )
    .expect("write creature b");
    write_training_data(&data_dir, &[(vec![0.5], vec![0.5]), (vec![1.0], vec![1.0])]);

    let output = Command::new(bin)
        .arg(&creatures_dir)
        .arg(&data_dir)
        .output()
        .expect("spawn scorer");
    assert!(
        output.status.success(),
        "directory mode should succeed, stderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout utf8");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("directory mode output JSON");
    assert!(parsed.get("alpha-1").is_some(), "missing key alpha-1");
    assert!(parsed.get("beta_2").is_some(), "missing key beta_2");
}

#[test]
fn directory_mode_rejects_shape_mismatch() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let tmp = tempfile::tempdir().expect("create tempdir");
    let creatures_dir = tmp.path().join("creatures");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir(&data_dir).expect("create data dir");

    std::fs::write(creatures_dir.join("a.json"), minimal_creature(1, 1, true)).expect("write a");
    std::fs::write(creatures_dir.join("b.json"), minimal_creature(2, 1, true)).expect("write b");
    write_training_data(&data_dir, &[(vec![0.5], vec![0.5])]);

    let output = Command::new(bin)
        .arg(&creatures_dir)
        .arg(&data_dir)
        .output()
        .expect("spawn scorer");
    assert!(!output.status.success(), "shape mismatch should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("must share the same shape"),
        "expected shape mismatch error, got: {stderr}",
    );
}

/// Issue #38: directory mode must produce the same per-creature scores when
/// the read buffer is a whole-record multiple (fast path: skip the
/// `pending.extend_from_slice` memcpy) as when it is forced unaligned (slow
/// path: bytes flow through the `pending` buffer). Identical results across
/// both buffer sizes mean the fast path preserves correctness.
#[test]
fn directory_mode_record_aligned_fast_path_matches_slow_path() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let tmp = tempfile::tempdir().expect("create tempdir");
    let creatures_dir = tmp.path().join("creatures");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir(&data_dir).expect("create data dir");

    std::fs::write(
        creatures_dir.join("alpha.json"),
        minimal_creature(1, 1, true),
    )
    .expect("write creature alpha");
    std::fs::write(
        creatures_dir.join("beta.json"),
        minimal_creature(1, 1, true),
    )
    .expect("write creature beta");

    // 1 input + 1 output = 8 bytes per record. 256 records = 2 KiB.
    let mut records: Vec<(Vec<f32>, Vec<f32>)> = Vec::with_capacity(256);
    for i in 0..256 {
        let v = (i as f32) * 1.0e-3;
        records.push((vec![v], vec![v]));
    }
    write_training_data(&data_dir, &records);

    // Aligned read buffer: 32 bytes = 4 records. Fast path is taken.
    let aligned = Command::new(bin)
        .env("NEAT_SCORER_READ_BYTES", "32")
        .arg(&creatures_dir)
        .arg(&data_dir)
        .output()
        .expect("spawn aligned scorer");
    assert!(
        aligned.status.success(),
        "aligned read should succeed, stderr:\n{}",
        String::from_utf8_lossy(&aligned.stderr),
    );

    // Smaller aligned buffer (single record). Forces many chunk callbacks
    // and exercises the per-record fast path.
    let single_record = Command::new(bin)
        .env("NEAT_SCORER_READ_BYTES", "8")
        .arg(&creatures_dir)
        .arg(&data_dir)
        .output()
        .expect("spawn single-record scorer");
    assert!(
        single_record.status.success(),
        "single-record read should succeed, stderr:\n{}",
        String::from_utf8_lossy(&single_record.stderr),
    );

    let aligned_json: serde_json::Value =
        serde_json::from_slice(&aligned.stdout).expect("aligned output JSON");
    let single_json: serde_json::Value =
        serde_json::from_slice(&single_record.stdout).expect("single-record output JSON");

    for key in ["alpha", "beta"] {
        let a = aligned_json
            .get(key)
            .unwrap_or_else(|| panic!("aligned missing key {key}"));
        let s = single_json
            .get(key)
            .unwrap_or_else(|| panic!("single missing key {key}"));
        let a_err = a
            .get("error")
            .and_then(|v| v.as_f64())
            .expect("aligned err");
        let s_err = s.get("error").and_then(|v| v.as_f64()).expect("single err");
        assert!(
            (a_err - s_err).abs() < 1e-9,
            "error mismatch for {key}: aligned={a_err} single={s_err}",
        );
        assert_eq!(a.get("recordCount"), s.get("recordCount"));
        assert_eq!(
            a.get("recordCount").and_then(|v| v.as_u64()).unwrap(),
            records.len() as u64,
        );
    }
}

#[test]
fn directory_mode_rejects_forward_only_false() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let tmp = tempfile::tempdir().expect("create tempdir");
    let creatures_dir = tmp.path().join("creatures");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir(&data_dir).expect("create data dir");

    std::fs::write(creatures_dir.join("a.json"), minimal_creature(1, 1, false)).expect("write a");
    write_training_data(&data_dir, &[(vec![0.5], vec![0.5])]);

    let output = Command::new(bin)
        .arg(&creatures_dir)
        .arg(&data_dir)
        .output()
        .expect("spawn scorer");
    assert!(!output.status.success(), "forwardOnly=false should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("forwardOnly=false"),
        "expected forwardOnly error, got: {stderr}",
    );
}
