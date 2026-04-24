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
