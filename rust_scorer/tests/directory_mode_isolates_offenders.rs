//! GRQ#4387 — one bad creature must cost one creature's score, not the batch.
//!
//! Before this, a single `.json` in the directory that would not compile
//! aborted the whole run: `rust_scorer` printed nothing on stdout and exited 1,
//! so the caller lost every other creature's score. GRQ-25 lost 23 creatures'
//! scores to one duplicate-synapse creature that way.
//!
//! The contract these tests pin:
//!
//! * every creature that *can* be scored still is;
//! * the offender is reported under its own filename stem as an entry carrying
//!   `failed: true`, a machine-readable `reason` and the scorer's own message;
//! * stdout stays a complete JSON map — no stem silently disappears;
//! * the exit code says "some creatures failed" (3), distinguishably from "the
//!   run failed" (1);
//! * a run in which *nothing* survived is still a run failure — isolation
//!   never quietly reconciles a dead batch to a green result.

use std::io::Write;
use std::path::Path;
use std::process::Command;

/// Exit status for "the batch completed, some creatures did not".
const EXIT_CREATURE_FAILURES: i32 = 3;

fn write_training_data(dir: &Path, records: &[(Vec<f32>, Vec<f32>)]) {
    let mut file = std::fs::File::create(dir.join("0.bin")).expect("create data file");
    for (inputs, outputs) in records {
        for &v in inputs.iter().chain(outputs.iter()) {
            file.write_all(&v.to_le_bytes()).expect("write f32");
        }
    }
}

/// A 1-input / 1-output creature that compiles and scores.
fn healthy_creature() -> String {
    r#"{"input":1,"output":1,"forwardOnly":true,"neurons":[{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}],"synapses":[{"fromUUID":"input-0","toUUID":"output-0","weight":1.0}]}"#.to_string()
}

/// The GRQ-25 shape: the same `(fromUUID, toUUID, type)` synapse twice, which
/// `compile_creature` refuses. Loads and parses fine — it only dies at compile,
/// which is exactly the fault that used to take the batch down.
fn duplicate_synapse_creature() -> String {
    r#"{"input":1,"output":1,"forwardOnly":true,"neurons":[{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}],"synapses":[{"fromUUID":"input-0","toUUID":"output-0","weight":1.0},{"fromUUID":"input-0","toUUID":"output-0","weight":0.5}]}"#.to_string()
}

struct Batch {
    _tmp: tempfile::TempDir,
    creatures_dir: std::path::PathBuf,
    data_dir: std::path::PathBuf,
}

fn batch(files: &[(&str, String)]) -> Batch {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let creatures_dir = tmp.path().join("creatures");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir(&data_dir).expect("create data dir");
    for (name, body) in files {
        std::fs::write(creatures_dir.join(format!("{name}.json")), body).expect("write creature");
    }
    write_training_data(&data_dir, &[(vec![0.5], vec![0.5]), (vec![1.0], vec![1.0])]);
    Batch {
        _tmp: tmp,
        creatures_dir,
        data_dir,
    }
}

fn run(batch: &Batch) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .arg(&batch.creatures_dir)
        .arg(&batch.data_dir)
        .output()
        .expect("spawn scorer")
}

/// Assert `stem` is a real score entry, not an offender.
fn assert_scored(parsed: &serde_json::Value, stem: &str) {
    let entry = parsed
        .get(stem)
        .unwrap_or_else(|| panic!("stem {stem} missing from the map: {parsed}"));
    assert!(
        entry.get("failed").is_none(),
        "{stem} must be a score entry, got: {entry}",
    );
    assert!(
        entry.get("score").and_then(|v| v.as_f64()).is_some(),
        "{stem} must carry a finite score, got: {entry}",
    );
}

/// Assert `stem` is an offender entry naming `reason`.
fn assert_failed(parsed: &serde_json::Value, stem: &str, reason: &str) {
    let entry = parsed
        .get(stem)
        .unwrap_or_else(|| panic!("stem {stem} missing from the map: {parsed}"));
    assert_eq!(
        entry.get("failed").and_then(|v| v.as_bool()),
        Some(true),
        "{stem} must be an offender entry, got: {entry}",
    );
    assert_eq!(
        entry.get("reason").and_then(|v| v.as_str()),
        Some(reason),
        "{stem} must carry reason {reason}, got: {entry}",
    );
    let message = entry
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("{stem} must carry a message, got: {entry}"));
    assert!(
        message.contains(stem),
        "the offender message must name the file, got: {message}",
    );
    assert!(
        entry.get("score").is_none(),
        "an offender must not be handed a score — a fabricated score is the \
         failure mode this whole change exists to avoid, got: {entry}",
    );
}

#[test]
fn one_uncompilable_creature_does_not_lose_the_other_scores() {
    let b = batch(&[
        ("alpha", healthy_creature()),
        ("poison", duplicate_synapse_creature()),
        ("beta", healthy_creature()),
    ]);
    let output = run(&b);

    assert_eq!(
        output.status.code(),
        Some(EXIT_CREATURE_FAILURES),
        "a partially-failed batch must exit {EXIT_CREATURE_FAILURES}, stderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );

    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must still be a complete JSON map");
    assert_eq!(
        parsed.as_object().map(|m| m.len()),
        Some(3),
        "every creature in the directory must appear, got: {parsed}",
    );
    assert_scored(&parsed, "alpha");
    assert_scored(&parsed, "beta");
    assert_failed(&parsed, "poison", "COMPILE");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("poison.json"),
        "stderr must still name the offending file, got:\n{stderr}",
    );
}

#[test]
fn an_unparseable_creature_is_isolated_too() {
    let b = batch(&[
        ("alpha", healthy_creature()),
        ("garbage", "{ not json at all".to_string()),
    ]);
    let output = run(&b);

    assert_eq!(output.status.code(), Some(EXIT_CREATURE_FAILURES));
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    assert_scored(&parsed, "alpha");
    assert_failed(&parsed, "garbage", "PARSE");
}

#[test]
fn a_shape_mismatch_is_isolated_against_the_batch_shape() {
    let b = batch(&[
        ("alpha", healthy_creature()),
        (
            "wide",
            r#"{"input":2,"output":1,"forwardOnly":true,"neurons":[{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}],"synapses":[{"fromUUID":"input-0","toUUID":"output-0","weight":1.0}]}"#.to_string(),
        ),
    ]);
    let output = run(&b);

    assert_eq!(output.status.code(), Some(EXIT_CREATURE_FAILURES));
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    assert_scored(&parsed, "alpha");
    assert_failed(&parsed, "wide", "SHAPE");
}

/// A zero-`input` creature (Issue #571's guard) is one creature's fault, so it
/// is isolated — but the guard itself is unchanged: the creature is never
/// scored, and never has its width re-derived from `neurons`.
///
/// `neat-core` rejects the width during `parse_creature_json`, so the isolated
/// entry is classified `PARSE`; the scorer's own `validate_creature_width` is
/// the second line of defence behind it. What matters here is the isolation and
/// the message, not which of the two guards fired.
#[test]
fn a_widthless_creature_is_isolated_and_never_scored() {
    let b = batch(&[
        ("alpha", healthy_creature()),
        (
            "widthless",
            r#"{"input":0,"output":1,"forwardOnly":true,"neurons":[{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}],"synapses":[{"fromUUID":"input-0","toUUID":"output-0","weight":1.0}]}"#.to_string(),
        ),
    ]);
    let output = run(&b);

    assert_eq!(output.status.code(), Some(EXIT_CREATURE_FAILURES));
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    assert_scored(&parsed, "alpha");

    let entry = parsed.get("widthless").expect("widthless entry");
    assert_eq!(entry.get("failed").and_then(|v| v.as_bool()), Some(true));
    assert!(
        matches!(
            entry.get("reason").and_then(|v| v.as_str()),
            Some("PARSE") | Some("WIDTH"),
        ),
        "a widthless creature must be isolated by one of the two width guards, got: {entry}",
    );
    assert!(
        entry
            .get("message")
            .and_then(|v| v.as_str())
            .is_some_and(|m| m.contains("at least one input")),
        "the offender message must carry the width error, got: {entry}",
    );
    assert!(
        entry.get("score").is_none(),
        "a widthless creature must never be handed a score, got: {entry}",
    );
}

/// The `#3815` boundary: "score the rest" must never become "quietly score
/// fewer". With nothing left to protect, the run fails exactly as it did
/// before — non-zero, no JSON, the offender's own message on stderr.
#[test]
fn a_batch_in_which_every_creature_fails_is_still_a_run_failure() {
    let b = batch(&[
        ("poison-a", duplicate_synapse_creature()),
        ("poison-b", duplicate_synapse_creature()),
    ]);
    let output = run(&b);

    assert_eq!(
        output.status.code(),
        Some(1),
        "a batch with no survivor must fail the run, not report a partial one",
    );
    assert!(
        output.stdout.is_empty(),
        "no JSON may be emitted when nothing was scored, got:\n{}",
        String::from_utf8_lossy(&output.stdout),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("poison-a.json"),
        "stderr must name the first offender, got:\n{stderr}",
    );
}

/// A clean batch is untouched: exit 0, no `failed` entries, and every creature
/// keeps the score fields the pre-GRQ#4387 contract promised.
#[test]
fn a_clean_batch_keeps_exit_zero_and_the_original_shape() {
    let b = batch(&[("alpha", healthy_creature()), ("beta", healthy_creature())]);
    let output = run(&b);

    assert!(
        output.status.success(),
        "a clean batch must still exit 0, stderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    assert_scored(&parsed, "alpha");
    assert_scored(&parsed, "beta");
    for stem in ["alpha", "beta"] {
        let entry = parsed.get(stem).expect("entry");
        for field in ["error", "recordCount", "hiddenNeurons", "costName"] {
            assert!(
                entry.get(field).is_some(),
                "{stem} lost the `{field}` field: {entry}",
            );
        }
    }
}

/// Scores must not shift because a sibling creature was dropped. The surviving
/// creature scores identically whether or not a poisonous creature shared the
/// directory — isolation removes the offender, it does not re-weight anyone.
#[test]
fn isolating_an_offender_does_not_move_the_survivors_scores() {
    let clean = batch(&[("alpha", healthy_creature())]);
    let poisoned = batch(&[
        ("alpha", healthy_creature()),
        ("poison", duplicate_synapse_creature()),
    ]);

    let clean_out = run(&clean);
    assert!(clean_out.status.success());
    let poisoned_out = run(&poisoned);
    assert_eq!(poisoned_out.status.code(), Some(EXIT_CREATURE_FAILURES));

    let clean_json: serde_json::Value = serde_json::from_slice(&clean_out.stdout).expect("JSON");
    let poisoned_json: serde_json::Value =
        serde_json::from_slice(&poisoned_out.stdout).expect("JSON");

    for field in ["score", "error", "recordCount"] {
        assert_eq!(
            clean_json.get("alpha").and_then(|e| e.get(field)),
            poisoned_json.get("alpha").and_then(|e| e.get(field)),
            "alpha's `{field}` moved when a poisonous sibling was isolated",
        );
    }
}
