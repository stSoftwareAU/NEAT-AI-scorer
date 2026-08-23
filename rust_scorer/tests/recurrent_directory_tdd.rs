//! Issue #579 — directory mode must score `forwardOnly: false` (recurrent)
//! creatures natively, with the same per-record reset semantics the
//! single-creature path already applies.
//!
//! Every fixture here is deliberately **dyadic**: inputs, weights and targets
//! are exact binary fractions, so each per-record error and every partial sum
//! is exact in `f64`. That makes the assertions independent of how the batch
//! path partitions a chunk across Rayon workers (which re-associates the f64
//! partial sums) — a directory-mode error may be compared bit-for-bit against
//! the single-creature error without a tolerance.

use std::io::Write;
use std::path::Path;
use std::process::Command;

/// Exact-binary inputs; targets are all `0.0`, so a record's error is the
/// square of the creature's output.
const INPUTS: [f32; 8] = [0.5, 1.0, 0.25, 0.75, 0.5, 0.25, 1.0, 0.75];

/// Mean squared error of the recurrent creature under **reset** semantics:
/// `output = 0.5 * input` (the back edge reads a cleared activation), so the
/// per-record error is `0.25 * input²`.
const RECURRENT_EXPECTED_ERROR: f64 = 0.117_187_5;

/// Mean squared error of the feed-forward creature: `output = 0.25 * input`,
/// so the per-record error is `0.062_5 * input²`.
const FORWARD_EXPECTED_ERROR: f64 = 0.029_296_875;

fn write_training_data(dir: &Path) {
    let mut file = std::fs::File::create(dir.join("0.bin")).expect("create data file");
    for &input in &INPUTS {
        file.write_all(&input.to_le_bytes()).expect("write input");
        file.write_all(&0.0_f32.to_le_bytes())
            .expect("write target");
    }
}

/// 1-in / 1-out creature carrying a genuine **back edge**: `output-0` (neuron
/// index 2) feeds `hidden-0` (neuron index 1), which the compiler evaluates
/// first. Under `forwardOnly: false` the activation buffer is cleared before
/// every record, so that edge contributes `0.0` and `output = 0.5 * input`.
/// Under `forwardOnly: true` it would instead read the previous record's
/// output — which is exactly the state leak this mode must not introduce.
fn recurrent_creature() -> String {
    r#"{"input":1,"output":1,"forwardOnly":false,"semanticVersion":"4.0.0","neurons":[{"type":"hidden","uuid":"hidden-0","bias":0.0,"squash":"IDENTITY"},{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}],"synapses":[{"fromUUID":"input-0","toUUID":"hidden-0","weight":0.5},{"fromUUID":"hidden-0","toUUID":"output-0","weight":1.0},{"fromUUID":"output-0","toUUID":"hidden-0","weight":0.5}]}"#
        .to_string()
}

/// Plain feed-forward creature sharing the recurrent fixture's 1-in / 1-out
/// shape (directory mode still requires one shape across the batch).
fn forward_creature() -> String {
    r#"{"input":1,"output":1,"forwardOnly":true,"semanticVersion":"4.0.0","neurons":[{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}],"synapses":[{"fromUUID":"input-0","toUUID":"output-0","weight":0.25}]}"#
        .to_string()
}

fn run_scorer(creature_path: &Path, data_dir: &Path) -> serde_json::Value {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let output = Command::new(bin)
        .arg(creature_path)
        .arg(data_dir)
        .output()
        .expect("spawn scorer");
    assert!(
        output.status.success(),
        "scorer must exit 0 for '{}', stderr:\n{}",
        creature_path.display(),
        String::from_utf8_lossy(&output.stderr),
    );
    serde_json::from_slice(&output.stdout).expect("stdout must be JSON")
}

fn field_f64(entry: &serde_json::Value, name: &str) -> f64 {
    entry
        .get(name)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or_else(|| panic!("missing numeric `{name}` in {entry}"))
}

/// Issue #579 — a directory containing a recurrent creature is scored, not
/// rejected, and the reported error is the reset-semantics value.
#[test]
fn directory_mode_scores_forward_only_false() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let creatures_dir = tmp.path().join("creatures");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir(&data_dir).expect("create data dir");
    std::fs::write(creatures_dir.join("rec.json"), recurrent_creature()).expect("write rec");
    write_training_data(&data_dir);

    let parsed = run_scorer(&creatures_dir, &data_dir);
    let entry = parsed
        .get("rec")
        .expect("missing `rec` key in directory JSON");

    assert_eq!(
        field_f64(entry, "error").to_bits(),
        RECURRENT_EXPECTED_ERROR.to_bits(),
        "recurrent creature must be scored with per-record state reset, got {entry}",
    );
    assert_eq!(
        entry
            .get("forwardOnly")
            .and_then(serde_json::Value::as_bool),
        Some(false),
        "the reported forwardOnly must be the creature's own flag, got {entry}",
    );
    assert_eq!(
        entry.get("recordCount").and_then(serde_json::Value::as_u64),
        Some(INPUTS.len() as u64),
        "every record must be scored, got {entry}",
    );
}

/// Issue #579 — the directory path and the single-creature path must agree
/// exactly for the same recurrent creature: one engine, one answer.
#[test]
fn directory_mode_recurrent_score_matches_single_creature_mode() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let creatures_dir = tmp.path().join("creatures");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir(&data_dir).expect("create data dir");
    let single_path = tmp.path().join("rec.json");
    std::fs::write(&single_path, recurrent_creature()).expect("write single");
    std::fs::write(creatures_dir.join("rec.json"), recurrent_creature()).expect("write rec");
    write_training_data(&data_dir);

    let single = run_scorer(&single_path, &data_dir);
    let batch = run_scorer(&creatures_dir, &data_dir);
    let entry = batch
        .get("rec")
        .expect("missing `rec` key in directory JSON");

    assert_eq!(
        field_f64(entry, "error").to_bits(),
        field_f64(&single, "error").to_bits(),
        "directory error must be bit-identical to single-creature error\nsingle={single}\nbatch={entry}",
    );
    assert_eq!(
        field_f64(entry, "score").to_bits(),
        field_f64(&single, "score").to_bits(),
        "directory score must be bit-identical to single-creature score",
    );
}

/// Issue #579 — a mixed batch: the recurrent creature keeps its reset
/// semantics while the forward-only creature beside it is unaffected, and both
/// match their single-creature runs.
#[test]
fn directory_mode_mixes_forward_only_and_recurrent_creatures() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let creatures_dir = tmp.path().join("creatures");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir(&data_dir).expect("create data dir");
    std::fs::write(creatures_dir.join("rec.json"), recurrent_creature()).expect("write rec");
    std::fs::write(creatures_dir.join("fwd.json"), forward_creature()).expect("write fwd");
    let single_rec = tmp.path().join("rec.json");
    let single_fwd = tmp.path().join("fwd.json");
    std::fs::write(&single_rec, recurrent_creature()).expect("write single rec");
    std::fs::write(&single_fwd, forward_creature()).expect("write single fwd");
    write_training_data(&data_dir);

    let batch = run_scorer(&creatures_dir, &data_dir);
    let rec = batch.get("rec").expect("missing `rec` key");
    let fwd = batch.get("fwd").expect("missing `fwd` key");

    assert_eq!(
        field_f64(rec, "error").to_bits(),
        RECURRENT_EXPECTED_ERROR.to_bits(),
        "recurrent creature in a mixed batch keeps reset semantics, got {rec}",
    );
    assert_eq!(
        field_f64(fwd, "error").to_bits(),
        FORWARD_EXPECTED_ERROR.to_bits(),
        "forward-only creature in a mixed batch is unaffected, got {fwd}",
    );
    assert_eq!(
        rec.get("forwardOnly").and_then(serde_json::Value::as_bool),
        Some(false),
        "recurrent entry must report forwardOnly=false, got {rec}",
    );
    assert_eq!(
        fwd.get("forwardOnly").and_then(serde_json::Value::as_bool),
        Some(true),
        "forward-only entry must report forwardOnly=true, got {fwd}",
    );

    let single_rec = run_scorer(&single_rec, &data_dir);
    let single_fwd = run_scorer(&single_fwd, &data_dir);
    assert_eq!(
        field_f64(rec, "error").to_bits(),
        field_f64(&single_rec, "error").to_bits(),
        "mixed-batch recurrent error must match its single-creature run",
    );
    assert_eq!(
        field_f64(fwd, "error").to_bits(),
        field_f64(&single_fwd, "error").to_bits(),
        "mixed-batch forward-only error must match its single-creature run",
    );
}
