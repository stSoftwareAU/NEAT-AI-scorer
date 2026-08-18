//! Issue #571 — `input < 1` / `output < 1` is never accepted on any scoring
//! path, and the rejection happens **before any training data is opened**.
//!
//! Every test below points the scorer at a training-data path that does not
//! exist. A path that read data first would fail with a `not a directory` /
//! `No .bin files` error; the width guard must fire ahead of that, with the
//! single shared wording from `rust_scorer::creature_width`.
//!
//! The wording asserted here is the TypeScript reference string
//! (`CreatureValidate.ts`) that `neat-core` will also emit once
//! NEAT-AI-core#550 lands, so these tests hold whether the JSON is rejected by
//! this crate's boundary guard or by the core parser.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use neat_core::creature::{
    CreatureExport, NeuronExport, SynapseExport, compile_creature, parse_creature_json,
};
use neat_core::training_data::TrainingDataConfig;
use rust_scorer::cost::{CostKind, accumulate_cost_sum};
use rust_scorer::creature_width::{
    CreatureWidthError, validate_creature_width, validate_observation_width,
};
use rust_scorer::fixture_json::{creature_envelope, neuron_json, synapse_json};
use rust_scorer::scoring::{ScoringError, compute_score_components};
use rust_scorer::stream_score::accumulate_cost_sum_forward_only_fused;

const INPUT_ZERO_MSG: &str = "Must have at least one input neurons was: 0";
const OUTPUT_ZERO_MSG: &str = "Must have at least one output neurons was: 0";

/// A one-output creature whose top-level counts are set explicitly, so a
/// zeroed `input` still carries the full (non-input) neuron list — the case
/// where a consumer that re-derived width from `neurons` would go wrong.
fn creature_json(input: usize, output: usize) -> String {
    let neurons = vec![neuron_json("output", "output-0", 0.0, "IDENTITY")];
    let synapses = vec![synapse_json("input-0", "output-0", 1.0)];
    creature_envelope(input, output, &neurons, &synapses)
}

fn scorer_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rust_scorer"))
}

fn missing_data_dir(tmp: &Path) -> PathBuf {
    let missing = tmp.join("no-such-data-dir");
    assert!(!missing.exists());
    missing
}

fn assert_rejected_before_data(output: &std::process::Output, expected: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "scorer must exit non-zero, stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains(expected),
        "stderr must carry the shared width error `{expected}`, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("is not a directory") && !stderr.contains("No .bin files"),
        "the width guard must fire before the data directory is touched, got:\n{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "no score JSON may be emitted for a widthless creature, got:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

// ---------------------------------------------------------------------------
// Single-creature path (`rust_scorer <creature.json> <data_dir>`)
// ---------------------------------------------------------------------------

#[test]
fn single_creature_path_rejects_zero_input_before_reading_data() {
    let tmp = tempfile::tempdir().unwrap();
    let creature = tmp.path().join("creature.json");
    std::fs::write(&creature, creature_json(0, 1)).unwrap();

    let output = Command::new(scorer_bin())
        .arg(&creature)
        .arg(missing_data_dir(tmp.path()))
        .output()
        .expect("spawn scorer");
    assert_rejected_before_data(&output, INPUT_ZERO_MSG);
}

#[test]
fn single_creature_path_rejects_zero_output_before_reading_data() {
    let tmp = tempfile::tempdir().unwrap();
    let creature = tmp.path().join("creature.json");
    std::fs::write(&creature, creature_json(1, 0)).unwrap();

    let output = Command::new(scorer_bin())
        .arg(&creature)
        .arg(missing_data_dir(tmp.path()))
        .output()
        .expect("spawn scorer");
    assert_rejected_before_data(&output, OUTPUT_ZERO_MSG);
}

#[test]
fn creature_stdin_path_rejects_zero_input_before_reading_data() {
    let tmp = tempfile::tempdir().unwrap();
    let mut child = Command::new(scorer_bin())
        .arg("--creature-stdin")
        .arg(missing_data_dir(tmp.path()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn scorer");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(creature_json(0, 1).as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_rejected_before_data(&output, INPUT_ZERO_MSG);
}

// ---------------------------------------------------------------------------
// Multi-creature directory path (`rust_scorer <creatures_dir> <data_dir>`)
// ---------------------------------------------------------------------------

#[test]
fn directory_path_rejects_zero_input_before_reading_data() {
    let tmp = tempfile::tempdir().unwrap();
    let creatures = tmp.path().join("creatures");
    std::fs::create_dir(&creatures).unwrap();
    std::fs::write(creatures.join("ok.json"), creature_json(1, 1)).unwrap();
    std::fs::write(creatures.join("widthless.json"), creature_json(0, 1)).unwrap();

    for gpu in ["off", "auto"] {
        let output = Command::new(scorer_bin())
            .arg("--gpu")
            .arg(gpu)
            .arg(&creatures)
            .arg(missing_data_dir(tmp.path()))
            .output()
            .expect("spawn scorer");
        assert_rejected_before_data(&output, INPUT_ZERO_MSG);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("widthless.json"),
            "directory mode must name the offending creature, got:\n{stderr}"
        );
    }
}

#[test]
fn directory_path_rejects_zero_output_before_reading_data() {
    let tmp = tempfile::tempdir().unwrap();
    let creatures = tmp.path().join("creatures");
    std::fs::create_dir(&creatures).unwrap();
    std::fs::write(creatures.join("widthless.json"), creature_json(1, 0)).unwrap();

    let output = Command::new(scorer_bin())
        .arg("--gpu")
        .arg("off")
        .arg(&creatures)
        .arg(missing_data_dir(tmp.path()))
        .output()
        .expect("spawn scorer");
    assert_rejected_before_data(&output, OUTPUT_ZERO_MSG);
}

// ---------------------------------------------------------------------------
// Fused streaming path (library API, `stream_score`)
// ---------------------------------------------------------------------------

fn compiled_identity() -> neat_core::network::CompiledNetwork {
    let creature = parse_creature_json(&creature_json(1, 1)).expect("valid creature parses");
    compile_creature(&creature).expect("valid creature compiles")
}

#[test]
fn streaming_path_rejects_zero_input_before_opening_any_bin() {
    let mut net = compiled_identity();
    let config = TrainingDataConfig {
        num_inputs: 0,
        num_outputs: 1,
    };
    // A `.bin` that does not exist: reaching the reader would fail with an
    // I/O error, not the width message.
    let bin_files = vec![PathBuf::from("/nonexistent/issue-571/0.bin")];
    let err = accumulate_cost_sum_forward_only_fused(CostKind::Mse, &bin_files, &config, &mut net)
        .expect_err("input:0 must be rejected");
    assert_eq!(err, INPUT_ZERO_MSG);
}

#[test]
fn streaming_path_rejects_zero_output_before_opening_any_bin() {
    let mut net = compiled_identity();
    let config = TrainingDataConfig {
        num_inputs: 1,
        num_outputs: 0,
    };
    let bin_files = vec![PathBuf::from("/nonexistent/issue-571/0.bin")];
    let err = accumulate_cost_sum_forward_only_fused(CostKind::Mse, &bin_files, &config, &mut net)
        .expect_err("output:0 must be rejected");
    assert_eq!(err, OUTPUT_ZERO_MSG);
}

// ---------------------------------------------------------------------------
// Cost dispatcher and complexity scoring (library API)
// ---------------------------------------------------------------------------

#[test]
fn cost_dispatcher_rejects_zero_widths() {
    let mut net = compiled_identity();
    let err = accumulate_cost_sum(CostKind::Mse, &mut net, &[0.5, 0.5], 0, 2, true)
        .expect_err("input_size 0 must be rejected");
    assert_eq!(err, INPUT_ZERO_MSG);
    let err = accumulate_cost_sum(CostKind::Mae, &mut net, &[0.5, 0.5], 2, 0, true)
        .expect_err("num_outputs 0 must be rejected");
    assert_eq!(err, OUTPUT_ZERO_MSG);
}

#[test]
fn score_components_reject_widthless_export() {
    // Struct literal, not parsed JSON, so this keeps testing the scorer's own
    // guard after `neat-core` starts rejecting the JSON (NEAT-AI-core#550).
    let creature = CreatureExport {
        input: 0,
        output: 1,
        neurons: vec![NeuronExport {
            neuron_type: "output".to_string(),
            uuid: "output-0".to_string(),
            bias: 0.0,
            squash: Some("IDENTITY".to_string()),
        }],
        synapses: vec![SynapseExport {
            from_uuid: "input-0".to_string(),
            to_uuid: "output-0".to_string(),
            weight: 1.0,
            synapse_type: None,
        }],
        semantic_version: Some("4.0.0".to_string()),
        forward_only: true,
    };
    let err = compute_score_components(&creature).expect_err("input:0 must be rejected");
    assert_eq!(
        err,
        ScoringError::InvalidObservationWidth(CreatureWidthError::InvalidInputCount { found: 0 })
    );
    assert_eq!(err.to_string(), INPUT_ZERO_MSG);

    let creature = CreatureExport {
        output: 0,
        input: 1,
        ..creature
    };
    assert_eq!(
        compute_score_components(&creature).unwrap_err().to_string(),
        OUTPUT_ZERO_MSG
    );
}

#[test]
fn shared_helper_is_the_single_source_of_the_wording() {
    assert_eq!(
        validate_observation_width(0, 7).unwrap_err().to_string(),
        INPUT_ZERO_MSG
    );
    assert_eq!(
        validate_observation_width(7, 0).unwrap_err().to_string(),
        OUTPUT_ZERO_MSG
    );
    let ok = parse_creature_json(&creature_json(3, 1)).unwrap();
    assert_eq!(validate_creature_width(&ok), Ok(()));
}

// ---------------------------------------------------------------------------
// A creature with valid counts still scores on both entry paths, and the
// record width it reads is `input + output` from the export.
// ---------------------------------------------------------------------------

fn write_records(dir: &Path, records: &[(Vec<f32>, Vec<f32>)]) {
    std::fs::create_dir_all(dir).unwrap();
    let mut file = std::fs::File::create(dir.join("0.bin")).unwrap();
    for (inputs, outputs) in records {
        for &v in inputs.iter().chain(outputs.iter()) {
            file.write_all(&v.to_le_bytes()).unwrap();
        }
    }
}

#[test]
fn valid_counts_still_score_on_single_and_directory_paths() {
    let tmp = tempfile::tempdir().unwrap();
    // 2 inputs + 1 output; only input-0 is wired, input-1 is a pure width
    // contribution (2 + 1 = 3 floats per record). Two records exactly.
    let json = creature_json(2, 1);
    let data = tmp.path().join("data");
    write_records(
        &data,
        &[(vec![0.5, 9.0], vec![0.5]), (vec![1.0, 9.0], vec![1.0])],
    );

    let creature = tmp.path().join("creature.json");
    std::fs::write(&creature, &json).unwrap();
    let output = Command::new(scorer_bin())
        .arg(&creature)
        .arg(&data)
        .output()
        .expect("spawn scorer");
    assert!(
        output.status.success(),
        "single-creature scoring must succeed, stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let single: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(single["recordCount"].as_u64(), Some(2));
    assert_eq!(single["error"].as_f64(), Some(0.0));

    let creatures = tmp.path().join("creatures");
    std::fs::create_dir(&creatures).unwrap();
    std::fs::write(creatures.join("a.json"), &json).unwrap();
    let output = Command::new(scorer_bin())
        .arg("--gpu")
        .arg("off")
        .arg(&creatures)
        .arg(&data)
        .output()
        .expect("spawn scorer");
    assert!(
        output.status.success(),
        "directory scoring must succeed, stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let multi: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(multi["a"]["recordCount"].as_u64(), Some(2));
    assert_eq!(multi["a"]["error"].as_f64(), Some(0.0));
}
