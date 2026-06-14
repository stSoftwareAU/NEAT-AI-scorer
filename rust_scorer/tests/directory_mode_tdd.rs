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

/// Forward-only creature with `hidden` TANH hidden neurons (total neuron count
/// = `input + hidden + output`). Used to build creatures above the 256-neuron
/// GPU shader cap.
fn large_creature(input: usize, output: usize, hidden: usize) -> String {
    let mut neurons = Vec::new();
    for h in 0..hidden {
        neurons.push(format!(
            r#"{{"type":"hidden","uuid":"hidden-{h}","bias":0.01,"squash":"TANH"}}"#
        ));
    }
    for o in 0..output {
        neurons.push(format!(
            r#"{{"type":"output","uuid":"output-{o}","bias":0.0,"squash":"IDENTITY"}}"#
        ));
    }
    let mut synapses = Vec::new();
    for i in 0..input {
        for h in 0..hidden {
            synapses.push(format!(
                r#"{{"fromUUID":"input-{i}","toUUID":"hidden-{h}","weight":0.02}}"#
            ));
        }
    }
    for h in 0..hidden {
        for o in 0..output {
            synapses.push(format!(
                r#"{{"fromUUID":"hidden-{h}","toUUID":"output-{o}","weight":0.03}}"#
            ));
        }
    }
    format!(
        r#"{{"input":{input},"output":{output},"forwardOnly":true,"neurons":[{}],"synapses":[{}]}}"#,
        neurons.join(","),
        synapses.join(","),
    )
}

/// Issue #180 / #182: `--gpu auto` (the default) over a directory whose creature
/// exceeds the 256-neuron private-array cap must complete *cleanly* — valid JSON
/// on stdout, exit 0. Before #180 the production regression aborted instead
/// (`exit 158` / `INVALID_JSON`). After #182, such creatures no longer force a
/// CPU fallback on hosts with a compatible GPU — they route to the
/// `forward_mse_scratch` kernel instead.
#[test]
fn gpu_auto_directory_above_private_cap_scores_cleanly() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let tmp = tempfile::tempdir().expect("create tempdir");
    let creatures_dir = tmp.path().join("creatures");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir(&data_dir).expect("create data dir");

    // 1 input + 300 hidden + 1 output = 302 neurons > 256-neuron shader cap.
    std::fs::write(creatures_dir.join("big.json"), large_creature(1, 1, 300))
        .expect("write big creature");
    write_training_data(&data_dir, &[(vec![0.5], vec![0.5]), (vec![1.0], vec![1.0])]);

    let output = Command::new(bin)
        .arg("--gpu")
        .arg("auto")
        .arg(&creatures_dir)
        .arg(&data_dir)
        .output()
        .expect("spawn scorer");
    assert!(
        output.status.success(),
        "auto fallback must exit 0, got status {:?}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("auto fallback must emit valid JSON on stdout");
    let entry = parsed.get("big").expect("missing key big");
    let gpu_backend = entry
        .get("gpuBackend")
        .and_then(|v| v.as_str())
        .expect("gpuBackend must be present");
    match gpu_backend {
        // CPU-only CI (no wgpu adapter) — pre-flight passes but auto falls back.
        "cpu-fallback" => {}
        // GPU host — Issue #182 routes above-cap creatures to the scratch kernel.
        _ => assert_eq!(
            entry.get("gpuKernel").and_then(|v| v.as_str()),
            Some("forward_mse_scratch"),
            "oversized creature on GPU must use the scratch kernel, got backend {gpu_backend}",
        ),
    }
    assert_eq!(
        entry.get("hiddenNeurons").and_then(|v| v.as_u64()),
        Some(300),
    );
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

/// Issue #42: directory mode used to call `compile_creature` once per
/// (creature × worker) pair — for a 2-creature run on a 16-CPU host that is
/// 32 compiles before any scoring runs. After the fix each creature is
/// compiled exactly once and the resulting `CompiledNetwork` is cloned for
/// any additional workers, so:
///
/// 1. `compileTimeSecs` must appear in the JSON for every creature, and
/// 2. it must be tightly bounded — well under the wall-clock budget that the
///    old "compile per worker" loop required for an even slightly non-trivial
///    creature.
#[test]
fn directory_mode_emits_compile_time_secs() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let tmp = tempfile::tempdir().expect("create tempdir");
    let creatures_dir = tmp.path().join("creatures");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir(&data_dir).expect("create data dir");

    std::fs::write(creatures_dir.join("a.json"), minimal_creature(1, 1, true)).expect("write a");
    std::fs::write(creatures_dir.join("b.json"), minimal_creature(1, 1, true)).expect("write b");
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

    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("directory mode output JSON");

    for key in ["a", "b"] {
        let entry = parsed
            .get(key)
            .unwrap_or_else(|| panic!("missing key {key}"));
        let compile_secs = entry
            .get("compileTimeSecs")
            .and_then(|v| v.as_f64())
            .unwrap_or_else(|| panic!("compileTimeSecs missing for {key}"));
        assert!(
            compile_secs >= 0.0,
            "compileTimeSecs must be non-negative for {key}, got {compile_secs}",
        );
        // Defensive upper bound: a single tiny-creature compile + clone is
        // sub-millisecond on every supported host. If the per-worker
        // recompile regresses, this will balloon well past the cap.
        assert!(
            compile_secs < 1.0,
            "compileTimeSecs unexpectedly large for {key}: {compile_secs}s",
        );
    }
}

/// Issue #205: under the default `--gpu auto`, a non-MSE `--cost` forces the
/// directory path onto CPU (`auto_should_use_gpu` returns false). That
/// fallback was otherwise silent — only the `gpuBackend: cpu-fallback` JSON
/// field hinted at it. Assert the CPU run now prints one informational stderr
/// note naming the cost as the reason, while MSE prints nothing extra.
#[test]
fn directory_mode_auto_non_mse_cost_notes_cpu_fallback() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let tmp = tempfile::tempdir().expect("create tempdir");
    let creatures_dir = tmp.path().join("creatures");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir(&data_dir).expect("create data dir");

    std::fs::write(creatures_dir.join("a.json"), minimal_creature(1, 1, true)).expect("write a");
    write_training_data(&data_dir, &[(vec![0.5], vec![0.5]), (vec![1.0], vec![1.0])]);

    // Non-MSE cost under the default `--gpu auto` ⇒ CPU fallback + stderr note.
    let non_mse = Command::new(bin)
        .arg("--cost")
        .arg("MAE")
        .arg(&creatures_dir)
        .arg(&data_dir)
        .output()
        .expect("spawn scorer (MAE)");
    assert!(
        non_mse.status.success(),
        "auto MAE directory run should succeed, stderr:\n{}",
        String::from_utf8_lossy(&non_mse.stderr),
    );
    let non_mse_stderr = String::from_utf8_lossy(&non_mse.stderr);
    assert!(
        non_mse_stderr.contains("[gpu] auto fallback to CPU directory mode")
            && non_mse_stderr.contains("MAE"),
        "non-MSE cost must print a CPU-fallback note naming the cost, got: {non_mse_stderr}",
    );

    // MSE (the GPU-supported cost) must not print the cost-fallback note.
    let mse = Command::new(bin)
        .arg("--cost")
        .arg("MSE")
        .arg(&creatures_dir)
        .arg(&data_dir)
        .output()
        .expect("spawn scorer (MSE)");
    assert!(
        mse.status.success(),
        "auto MSE directory run should succeed, stderr:\n{}",
        String::from_utf8_lossy(&mse.stderr),
    );
    let mse_stderr = String::from_utf8_lossy(&mse.stderr);
    assert!(
        !mse_stderr.contains("cost MSE is not GPU-supported"),
        "MSE must not print the cost CPU-fallback note, got: {mse_stderr}",
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
