//! End-to-end CLI coverage for record-level `--sample-rate` sub-sampling
//! (Issue #310, multi-fidelity fitness).
//!
//! Drives the real `rust_scorer` binary against a temp corpus and asserts the
//! forward-only streaming reader deterministically skips records: the scored
//! `recordCount` shrinks by the sample rate, the `sampleRate` field echoes the
//! effective rate, and repeated runs are bit-identical. The CPU directory path
//! (`rust_scorer <creatures_dir> <data_dir>`) is production's hot path, so it is
//! covered alongside the single-creature path.

use std::io::Write;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

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

/// A deterministic corpus of `n` single-input/single-output records where the
/// target deliberately differs from the (identity) prediction so the mean error
/// depends on *which* records are scored — this makes an accidental "sample but
/// score everything" bug observable.
fn varied_corpus(n: usize) -> Vec<(Vec<f32>, Vec<f32>)> {
    (0..n)
        .map(|i| {
            let x = i as f32 * 0.01;
            (vec![x], vec![x + (i % 3) as f32])
        })
        .collect()
}

fn run_single(bin: &str, creature: &Path, data_dir: &Path, extra: &[&str]) -> std::process::Output {
    Command::new(bin)
        .arg(creature)
        .arg(data_dir)
        .args(extra)
        .arg("--gpu")
        .arg("off")
        .output()
        .expect("run rust_scorer")
}

fn parse_stdout(output: &std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "scorer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is valid JSON")
}

#[test]
fn single_creature_half_rate_scores_half_the_records() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&data_dir).expect("data dir");
    write_training_data(&data_dir, &varied_corpus(100));
    let creature = tmp.path().join("c.json");
    std::fs::write(&creature, minimal_creature(1, 1, true)).expect("creature");

    let full = parse_stdout(&run_single(
        bin,
        &creature,
        &data_dir,
        &["--sample-rate", "1"],
    ));
    assert_eq!(full["recordCount"].as_u64(), Some(100));
    // Full-rate must not advertise a sampleRate (JSON stays pre-#310 shape).
    assert!(full.get("sampleRate").is_none());

    let half = parse_stdout(&run_single(
        bin,
        &creature,
        &data_dir,
        &["--sample-rate", "0.5"],
    ));
    assert_eq!(
        half["recordCount"].as_u64(),
        Some(50),
        "0.5 rate must score floor(100*0.5)=50 records"
    );
    assert_eq!(half["sampleRate"].as_f64(), Some(0.5));
    // The error is over the sampled subset, so it should differ from the full
    // corpus error (the corpus was built so the two strata differ).
    assert!(
        (half["error"].as_f64().unwrap() - full["error"].as_f64().unwrap()).abs() > 1e-9,
        "sampled error should reflect only the kept subset"
    );
}

#[test]
fn directory_mode_quarter_rate_is_deterministic_and_cuts_record_count() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let tmp = tempfile::tempdir().expect("tempdir");
    let creatures_dir = tmp.path().join("creatures");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&creatures_dir).expect("creatures dir");
    std::fs::create_dir(&data_dir).expect("data dir");
    std::fs::write(creatures_dir.join("a.json"), minimal_creature(1, 1, true)).expect("a");
    std::fs::write(creatures_dir.join("b.json"), minimal_creature(1, 1, true)).expect("b");
    write_training_data(&data_dir, &varied_corpus(200));

    let run = || {
        let out = Command::new(bin)
            .arg(&creatures_dir)
            .arg(&data_dir)
            .arg("--sample-rate")
            .arg("0.25")
            .arg("--gpu")
            .arg("off")
            .output()
            .expect("run rust_scorer");
        parse_stdout(&out)
    };

    let first = run();
    let second = run();
    // Compare only the deterministic scoring fields — `timeTaken` /
    // `compileTimeSecs` are wall-clock and legitimately vary run to run.
    for key in ["a", "b"] {
        for field in ["error", "score", "recordCount", "sampleRate"] {
            assert_eq!(
                first[key][field], second[key][field],
                "{key}.{field} must be deterministic across runs"
            );
        }
    }

    // floor(200 * 0.25) = 50 kept records for every creature.
    assert_eq!(first["a"]["recordCount"].as_u64(), Some(50));
    assert_eq!(first["b"]["recordCount"].as_u64(), Some(50));
    assert_eq!(first["a"]["sampleRate"].as_f64(), Some(0.25));
}

#[test]
fn phase_selects_a_different_subsample_than_default() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&data_dir).expect("data dir");
    write_training_data(&data_dir, &varied_corpus(100));
    let creature = tmp.path().join("c.json");
    std::fs::write(&creature, minimal_creature(1, 1, true)).expect("creature");

    let phase0 = parse_stdout(&run_single(
        bin,
        &creature,
        &data_dir,
        &["--sample-rate", "0.5", "--sample-phase", "0"],
    ));
    let phase1 = parse_stdout(&run_single(
        bin,
        &creature,
        &data_dir,
        &["--sample-rate", "0.5", "--sample-phase", "1"],
    ));

    // Same number of records, but a different (complementary) stratum → the mean
    // error over the two subsets differs.
    assert_eq!(phase0["recordCount"], phase1["recordCount"]);
    assert_ne!(
        phase0["error"].as_f64(),
        phase1["error"].as_f64(),
        "a different phase must select a different subsample"
    );
}

#[test]
fn out_of_range_sample_rate_is_rejected() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&data_dir).expect("data dir");
    write_training_data(&data_dir, &varied_corpus(10));
    let creature = tmp.path().join("c.json");
    std::fs::write(&creature, minimal_creature(1, 1, true)).expect("creature");

    for bad in ["0", "-0.5", "1.5", "2"] {
        let out = run_single(bin, &creature, &data_dir, &["--sample-rate", bad]);
        assert!(
            !out.status.success(),
            "--sample-rate {bad} must fail loud, not silently clamp"
        );
    }
}

#[test]
fn help_advertises_sample_rate_for_consumer_probe() {
    // The consumer (NEAT-AI#3257) probes `--help` for `--sample-rate` before
    // passing it on the batch path, so the flag must appear in help output.
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let out = Command::new(bin)
        .arg("--help")
        .output()
        .expect("run --help");
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("--sample-rate"),
        "help must advertise --sample-rate for the consumer probe"
    );
}
