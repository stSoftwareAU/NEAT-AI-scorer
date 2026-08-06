//! Issue #537 — directory-mode task-granularity multiplier
//! (`NEAT_SCORER_WORKER_SPLIT`).
//!
//! Splitting each creature's chunk into `k` record sub-ranges hands the Rayon
//! pool `k` times as many tasks, so the tail of every chunk idles fewer
//! threads. These tests pin the observable contract: the split is invisible in
//! the scores (bar f64 re-association at the rounding level), every creature is
//! still reported, and a malformed override falls back loudly to the default.

use std::io::Write;
use std::path::Path;
use std::process::Command;

/// Forward-only creature with `hidden` TANH neurons fed by every input — big
/// enough that a chunk split is a real partition of work, small enough to stay
/// inside the unit-test speed budget.
fn creature_json(input: usize, hidden: usize, weight_scale: f64) -> String {
    let mut neurons = Vec::new();
    let mut synapses = Vec::new();
    for h in 0..hidden {
        neurons.push(format!(
            r#"{{"type":"hidden","uuid":"hidden-{h}","bias":0.01,"squash":"TANH"}}"#
        ));
        for i in 0..input {
            let w = weight_scale * (1.0 + (h + i) as f64 * 0.01);
            synapses.push(format!(
                r#"{{"fromUUID":"input-{i}","toUUID":"hidden-{h}","weight":{w}}}"#
            ));
        }
        synapses.push(format!(
            r#"{{"fromUUID":"hidden-{h}","toUUID":"output-0","weight":0.03}}"#
        ));
    }
    neurons
        .push(r#"{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}"#.to_string());
    format!(
        r#"{{"input":{input},"output":1,"forwardOnly":true,"neurons":[{}],"synapses":[{}]}}"#,
        neurons.join(","),
        synapses.join(","),
    )
}

fn write_corpus(dir: &Path, num_inputs: usize, records: usize) {
    let mut file = std::fs::File::create(dir.join("0.bin")).expect("create data file");
    let mut buf: Vec<u8> = Vec::with_capacity(records * (num_inputs + 1) * 4);
    for r in 0..records {
        for i in 0..num_inputs {
            let v = ((r * 7 + i * 13) % 97) as f32 / 97.0;
            buf.extend_from_slice(&v.to_le_bytes());
        }
        let target = (r % 11) as f32 / 11.0;
        buf.extend_from_slice(&target.to_le_bytes());
    }
    file.write_all(&buf).expect("write corpus");
}

/// Build a directory of `n` distinct creatures plus a matching corpus.
fn fixture(
    tmp: &Path,
    n: usize,
    num_inputs: usize,
    records: usize,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let creatures_dir = tmp.join("creatures");
    let data_dir = tmp.join("data");
    std::fs::create_dir_all(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    for c in 0..n {
        // Distinct weights per creature — never the same network N times.
        let json = creature_json(num_inputs, 6, 0.02 + c as f64 * 0.003);
        std::fs::write(creatures_dir.join(format!("c-{c:02}.json")), json).expect("write creature");
    }
    write_corpus(&data_dir, num_inputs, records);
    (creatures_dir, data_dir)
}

fn score_with_split(creatures_dir: &Path, data_dir: &Path, split: &str) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .env("NEAT_SCORER_WORKER_SPLIT", split)
        .arg("--gpu")
        .arg("off")
        .arg(creatures_dir)
        .arg(data_dir)
        .output()
        .expect("spawn scorer");
    assert!(
        output.status.success(),
        "NEAT_SCORER_WORKER_SPLIT={split} must score cleanly, status {:?}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    serde_json::from_slice(&output.stdout).expect("scorer stdout must be JSON")
}

/// Issue #537: a split run must score every creature over the whole corpus and
/// land on the same error as the unsplit run — the partial sums are folded in a
/// fixed worker order, so only f64 re-association at the rounding level moves.
#[test]
fn split_run_matches_unsplit_scores() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let (creatures_dir, data_dir) = fixture(tmp.path(), 8, 4, 500);

    let baseline = score_with_split(&creatures_dir, &data_dir, "1");
    for split in ["2", "4", "8"] {
        let split_run = score_with_split(&creatures_dir, &data_dir, split);
        let base_map = baseline.as_object().expect("baseline object");
        let split_map = split_run.as_object().expect("split object");
        assert_eq!(
            base_map.len(),
            split_map.len(),
            "split={split} must report every creature"
        );
        for (key, base) in base_map {
            let got = split_map.get(key).expect("creature present in split run");
            assert_eq!(
                base["recordCount"], got["recordCount"],
                "split={split} must score creature '{key}' over the whole corpus"
            );
            let (a, b) = (
                base["error"].as_f64().expect("baseline error"),
                got["error"].as_f64().expect("split error"),
            );
            let rel = (a - b).abs() / a.abs().max(f64::MIN_POSITIVE);
            assert!(
                rel < 1e-9,
                "split={split} creature '{key}' error drifted beyond f64 re-association: {a} vs {b} (rel {rel:e})"
            );
        }
    }
}

/// Issue #537: the split must hold when the population is smaller than the
/// thread budget too — that path already partitioned records, and multiplying
/// it must not double-count or drop any.
#[test]
fn split_run_matches_unsplit_scores_for_small_population() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let (creatures_dir, data_dir) = fixture(tmp.path(), 2, 4, 300);

    let baseline = score_with_split(&creatures_dir, &data_dir, "1");
    let split_run = score_with_split(&creatures_dir, &data_dir, "4");
    for (key, base) in baseline.as_object().expect("baseline object") {
        let got = &split_run[key];
        assert_eq!(base["recordCount"], got["recordCount"]);
        let (a, b) = (
            base["error"].as_f64().expect("baseline error"),
            got["error"].as_f64().expect("split error"),
        );
        assert!(
            (a - b).abs() / a.abs().max(f64::MIN_POSITIVE) < 1e-9,
            "creature '{key}' error drifted: {a} vs {b}"
        );
    }
}

/// Issue #537: a malformed override must not fail silently — it warns on stderr
/// and falls back to the shipped `k = 1` granularity.
#[test]
fn malformed_split_warns_and_falls_back_to_default() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let (creatures_dir, data_dir) = fixture(tmp.path(), 3, 4, 120);

    let output = Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .env("NEAT_SCORER_WORKER_SPLIT", "four")
        .arg("--gpu")
        .arg("off")
        .arg(&creatures_dir)
        .arg(&data_dir)
        .output()
        .expect("spawn scorer");
    assert!(
        output.status.success(),
        "malformed override must still score"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("NEAT_SCORER_WORKER_SPLIT"),
        "a malformed override must warn loudly, stderr was:\n{stderr}"
    );

    let warned: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("scorer stdout must be JSON");
    let baseline = score_with_split(&creatures_dir, &data_dir, "1");
    for (key, base) in baseline.as_object().expect("baseline object") {
        // Bit-identical scores: the fallback must run the default partition,
        // not some other granularity (timing fields naturally differ).
        assert_eq!(
            base["error"], warned[key]["error"],
            "a malformed override must fall back to the default granularity for '{key}'"
        );
    }
}

/// Issue #537: zero is not a granularity — it must be rejected like any other
/// invalid value rather than producing an empty worker pool.
#[test]
fn zero_split_falls_back_to_default() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let (creatures_dir, data_dir) = fixture(tmp.path(), 3, 4, 120);

    let zeroed = score_with_split(&creatures_dir, &data_dir, "0");
    let baseline = score_with_split(&creatures_dir, &data_dir, "1");
    for (key, base) in baseline.as_object().expect("baseline object") {
        assert_eq!(
            base["recordCount"], zeroed[key]["recordCount"],
            "creature '{key}' must still be scored over the whole corpus"
        );
    }
}
