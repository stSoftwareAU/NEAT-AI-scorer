//! End-to-end tests for `--race-stdio` (NEAT-AI#3928).
//!
//! Issue #308 put the early-exit hook behind a **library** entrypoint, so a
//! caller that drives `rust_scorer` as a subprocess — every NEAT-AI deployment
//! does — could not reach it. These tests drive the compiled binary over the
//! stdio protocol that closes that gap, and assert the three contracts a
//! consumer depends on:
//!
//! * always answering `continue` reproduces plain directory-mode scores exactly;
//! * an `abort` verdict freezes that creature at its partial `recordCount`,
//!   leaving every other creature's full-corpus score untouched;
//! * a protocol fault (closed stdin, malformed verdict) exits non-zero instead
//!   of silently completing the full sweep.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const INPUTS: usize = 2;
const OUTPUTS: usize = 1;
const RECORD_BYTES: usize = (INPUTS + OUTPUTS) * 4;
const RECORDS: usize = 256;

/// Forward-only creature computing `w0*x0 + w1*x1` into one IDENTITY output.
fn linear_creature(w0: f32, w1: f32) -> String {
    format!(
        r#"{{"input":{INPUTS},"output":{OUTPUTS},"forwardOnly":true,"neurons":[{{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}}],"synapses":[{{"fromUUID":"input-0","toUUID":"output-0","weight":{w0}}},{{"fromUUID":"input-1","toUUID":"output-0","weight":{w1}}}]}}"#
    )
}

/// Build a `creatures/` + `data/` fixture pair under a fresh temp root.
fn fixture(tag: &str, n_creatures: usize) -> (PathBuf, PathBuf, tempfile::TempDir) {
    let root = tempfile::Builder::new()
        .prefix(&format!("racing_stdio_{tag}_"))
        .tempdir()
        .expect("create temp root");
    let creatures = root.path().join("creatures");
    let data = root.path().join("data");
    std::fs::create_dir_all(&creatures).expect("create creatures dir");
    std::fs::create_dir_all(&data).expect("create data dir");
    for i in 0..n_creatures {
        std::fs::write(
            creatures.join(format!("creature-{i:03}.json")),
            // Creature 0 is closest to the ground truth below, so the racing
            // policy in these tests has a stable best and stable losers.
            linear_creature(0.4 + 0.4 * i as f32, 0.5),
        )
        .expect("write creature");
    }
    let mut file = std::fs::File::create(data.join("0.bin")).expect("create data file");
    for r in 0..RECORDS {
        let x0 = (r as f32 * 0.01).sin();
        let x1 = (r as f32 * 0.017).cos();
        let target = 0.4 * x0 + 0.5 * x1;
        for v in [x0, x1, target] {
            file.write_all(&v.to_le_bytes()).expect("write f32");
        }
    }
    (creatures, data, root)
}

/// Score the directory without racing — the parity baseline.
fn score_plain(creatures: &Path, data: &Path) -> BTreeMap<String, serde_json::Value> {
    let out = Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .arg("--gpu")
        .arg("off")
        .arg(creatures)
        .arg(data)
        .env("NEAT_SCORER_READ_BYTES", RECORD_BYTES.to_string())
        .output()
        .expect("run plain directory scorer");
    assert!(
        out.status.success(),
        "plain scorer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("plain scorer emits a JSON map")
}

/// Outcome of one racing run.
struct RaceRun {
    status_success: bool,
    stderr: String,
    /// Parsed result map, when the run produced one.
    results: Option<BTreeMap<String, serde_json::Value>>,
    /// Chunk events observed on stdout.
    events: Vec<serde_json::Value>,
}

/// Drive `--race-stdio`, answering each chunk event with `decide(event)`.
///
/// `decide` returns the verdict line to write, or `None` to close stdin (the
/// protocol-fault case).
fn race<F>(creatures: &Path, data: &Path, mut decide: F) -> RaceRun
where
    F: FnMut(&serde_json::Value) -> Option<String>,
{
    let mut child = Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .arg("--gpu")
        .arg("off")
        .arg("--race-stdio")
        .arg(creatures)
        .arg(data)
        .env("NEAT_SCORER_READ_BYTES", RECORD_BYTES.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn racing scorer");

    let mut stdin = Some(child.stdin.take().expect("racing stdin"));
    let stdout = BufReader::new(child.stdout.take().expect("racing stdout"));

    let mut events = Vec::new();
    let mut tail = String::new();
    for line in stdout.lines() {
        let line = line.expect("read racing stdout line");
        if line.starts_with("{\"racing\"") {
            let event: serde_json::Value =
                serde_json::from_str(&line).expect("chunk event is one JSON object");
            let verdict = decide(&event);
            events.push(event);
            match verdict {
                // `None` closes the verdict stream — the protocol-fault case.
                None => stdin = None,
                Some(reply) => {
                    if let Some(pipe) = stdin.as_mut() {
                        // A scorer that has already stopped reading makes this
                        // write fail; that is the run ending, not a test fault.
                        if writeln!(pipe, "{reply}").is_err() || pipe.flush().is_err() {
                            stdin = None;
                        }
                    }
                }
            }
        } else {
            tail.push_str(&line);
            tail.push('\n');
        }
    }
    drop(stdin);
    let out = child.wait_with_output().expect("await racing scorer");
    RaceRun {
        status_success: out.status.success(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        results: serde_json::from_str(tail.trim()).ok(),
        events,
    }
}

fn error_of(results: &BTreeMap<String, serde_json::Value>, key: &str) -> f64 {
    results[key]["error"].as_f64().expect("error is a number")
}

fn record_count_of(results: &BTreeMap<String, serde_json::Value>, key: &str) -> u64 {
    results[key]["recordCount"]
        .as_u64()
        .expect("recordCount is an integer")
}

#[test]
fn continue_every_chunk_reproduces_plain_directory_scores() {
    let (creatures, data, _root) = fixture("parity", 3);
    let baseline = score_plain(&creatures, &data);
    let run = race(&creatures, &data, |_| {
        Some("{\"verdict\":\"continue\"}".to_string())
    });
    assert!(run.status_success, "racing run failed: {}", run.stderr);
    assert!(!run.events.is_empty(), "no chunk events were published");
    let raced = run.results.expect("racing run emits a result map");
    assert_eq!(raced.len(), baseline.len());
    for (key, value) in &baseline {
        assert_eq!(
            raced[key]["error"], value["error"],
            "error for {key} must be bit-identical to the non-racing sweep"
        );
        assert_eq!(
            raced[key]["score"], value["score"],
            "score for {key} must be bit-identical to the non-racing sweep"
        );
        assert_eq!(raced[key]["recordCount"], value["recordCount"]);
    }
}

#[test]
fn chunk_events_carry_the_running_partials_for_active_creatures() {
    let (creatures, data, _root) = fixture("events", 2);
    let run = race(&creatures, &data, |_| {
        Some("{\"verdict\":\"continue\"}".to_string())
    });
    assert!(run.status_success, "racing run failed: {}", run.stderr);
    let first = &run.events[0];
    assert_eq!(first["racing"], "chunk");
    assert_eq!(first["chunk"], 1);
    let partials = first["partials"].as_array().expect("partials array");
    assert_eq!(partials.len(), 2, "both creatures are active on chunk 1");
    for p in partials {
        assert!(p["key"].as_str().expect("key").starts_with("creature-"));
        assert!(p["partialError"].as_f64().expect("partialError") >= 0.0);
        assert!(p["recordsScored"].as_u64().expect("recordsScored") >= 1);
    }
    // Records scored must grow monotonically as the sweep advances.
    let last = run.events.last().expect("at least one event");
    let first_records = partials[0]["recordsScored"].as_u64().unwrap();
    let last_records = last["partials"][0]["recordsScored"].as_u64().unwrap();
    assert!(
        last_records > first_records,
        "recordsScored must advance across chunks ({first_records} -> {last_records})"
    );
}

#[test]
fn aborting_a_creature_freezes_it_at_its_partial_record_count() {
    let (creatures, data, _root) = fixture("abort", 3);
    let baseline = score_plain(&creatures, &data);
    let mut aborted_at: Option<u64> = None;
    let run = race(&creatures, &data, |event| {
        let partials = event["partials"].as_array().expect("partials");
        // Abandon the worst creature ("creature-002") on the first chunk only.
        if aborted_at.is_none() {
            let victim = partials
                .iter()
                .find(|p| p["key"] == "creature-002")
                .expect("victim present on chunk 1");
            aborted_at = Some(victim["recordsScored"].as_u64().unwrap());
            let index = victim["index"].as_u64().unwrap();
            return Some(format!("{{\"verdict\":\"abort\",\"creatures\":[{index}]}}"));
        }
        assert!(
            partials.iter().all(|p| p["key"] != "creature-002"),
            "an abandoned creature must not appear in later chunk events"
        );
        Some("{\"verdict\":\"continue\"}".to_string())
    });
    assert!(run.status_success, "racing run failed: {}", run.stderr);
    let raced = run.results.expect("racing run emits a result map");
    let frozen_at = aborted_at.expect("the victim was abandoned");

    assert_eq!(
        record_count_of(&raced, "creature-002"),
        frozen_at,
        "the abandoned creature freezes at the records it had scored"
    );
    assert!(
        record_count_of(&raced, "creature-002") < record_count_of(&baseline, "creature-002"),
        "the abandoned creature must not be scored over the whole corpus"
    );
    for survivor in ["creature-000", "creature-001"] {
        assert_eq!(
            error_of(&raced, survivor),
            error_of(&baseline, survivor),
            "{survivor} was never abandoned and keeps its exact full-corpus error"
        );
        assert_eq!(
            record_count_of(&raced, survivor),
            record_count_of(&baseline, survivor)
        );
    }
}

#[test]
fn abort_all_stops_the_sweep_for_every_creature() {
    let (creatures, data, _root) = fixture("abortall", 2);
    let baseline = score_plain(&creatures, &data);
    let run = race(&creatures, &data, |_| {
        Some("{\"verdict\":\"abortAll\"}".to_string())
    });
    assert!(run.status_success, "racing run failed: {}", run.stderr);
    assert_eq!(run.events.len(), 1, "abortAll stops after the first chunk");
    let raced = run.results.expect("racing run emits a result map");
    for key in raced.keys() {
        assert!(
            record_count_of(&raced, key) < record_count_of(&baseline, key),
            "{key} must freeze mid-corpus under abortAll"
        );
    }
}

#[test]
fn a_closed_verdict_stream_exits_non_zero() {
    let (creatures, data, _root) = fixture("closed", 2);
    let run = race(&creatures, &data, |_| None);
    assert!(
        !run.status_success,
        "a closed verdict stream must fail loud, not silently full-score"
    );
    assert!(
        run.stderr.contains("verdict stream closed"),
        "stderr must name the protocol fault, got: {}",
        run.stderr
    );
}

#[test]
fn a_malformed_verdict_exits_non_zero() {
    let (creatures, data, _root) = fixture("malformed", 2);
    let run = race(&creatures, &data, |_| Some("{\"verdict\":42}".to_string()));
    assert!(!run.status_success, "a malformed verdict must fail loud");
    assert!(
        run.stderr.contains("unparseable verdict"),
        "stderr must name the protocol fault, got: {}",
        run.stderr
    );
}

#[test]
fn race_stdio_rejects_a_single_creature_file() {
    let (creatures, data, _root) = fixture("single", 1);
    let creature_file = creatures.join("creature-000.json");
    let out = Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .arg("--gpu")
        .arg("off")
        .arg("--race-stdio")
        .arg(&creature_file)
        .arg(&data)
        .output()
        .expect("run scorer");
    assert!(!out.status.success(), "a file target must be rejected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--race-stdio requires a creatures directory"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn race_stdio_rejects_gpu_on() {
    let (creatures, data, _root) = fixture("gpuon", 2);
    let out = Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .arg("--gpu")
        .arg("on")
        .arg("--race-stdio")
        .arg(&creatures)
        .arg(&data)
        .output()
        .expect("run scorer");
    assert!(!out.status.success(), "--gpu on must be rejected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--race-stdio has no GPU kernel"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn help_advertises_race_stdio_so_callers_can_probe_for_it() {
    let out = Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .arg("--help")
        .output()
        .expect("run scorer --help");
    let help = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        help.contains("--race-stdio"),
        "--help must advertise --race-stdio for capability probing, got:\n{help}"
    );
}
