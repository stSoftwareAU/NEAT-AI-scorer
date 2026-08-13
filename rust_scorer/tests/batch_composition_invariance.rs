//! A creature's score is a function of the creature and the corpus — never of
//! the batch it was scored in (`stSoftwareAU/NEAT-AI-Lamarck#130`).
//!
//! Directory mode used to partition each creature's chunk into
//! `max(activation_threads, n_creatures) * split / n_creatures` record
//! sub-ranges, so the same creature's f64 partial sums were grouped differently
//! — and a different 8-record / 4-record / scalar SIMD path was selected in the
//! upstream loss kernels — purely because of how many *other* creatures shared
//! the call. On the production creature that moved the score by `1.755e-7`
//! relative, 175x the "~1e-9 relative in practice, never more" bound the module
//! documented, and it moved the incumbent and a candidate by different amounts,
//! so it perturbed the very Δ a caller decides an accept on.
//!
//! These tests pin the contract as **bit-identical**: a score that only has to
//! land "close" is a score a caller cannot subtract at `1e-6`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Forward-only creature with `hidden` TANH neurons fed by every input. Large
/// enough that a chunk is a real partition of work, small enough for the unit
/// test speed budget.
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
            let v = ((r * 13 + i * 7) % 89) as f32 / 89.0;
            buf.extend_from_slice(&v.to_le_bytes());
        }
        let target = (r % 7) as f32 / 7.0;
        buf.extend_from_slice(&target.to_le_bytes());
    }
    file.write_all(&buf).expect("write corpus");
}

const NUM_INPUTS: usize = 6;
const RECORDS: usize = 1500;

/// Build `n` distinct creatures. `creature-00` is the one every directory
/// shares — the "incumbent" whose score must not move.
fn creature_dir(root: &Path, name: &str, n: usize) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("create creature dir");
    for c in 0..n {
        let json = creature_json(NUM_INPUTS, 5, 0.02 + c as f64 * 0.004);
        std::fs::write(dir.join(format!("creature-{c:02}.json")), json).expect("write creature");
    }
    dir
}

/// Score a directory, optionally overriding the host worker knobs.
fn score(creatures_dir: &Path, data_dir: &Path, env: &[(&str, &str)]) -> serde_json::Value {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rust_scorer"));
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd
        .arg("--gpu")
        .arg("off")
        .arg(creatures_dir)
        .arg(data_dir)
        .output()
        .expect("spawn scorer");
    assert!(
        output.status.success(),
        "scorer must succeed for {}, status {:?}\nstderr:\n{}",
        creatures_dir.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    serde_json::from_slice(&output.stdout).expect("scorer stdout must be JSON")
}

fn field(scores: &serde_json::Value, stem: &str, field: &str) -> f64 {
    scores[stem][field]
        .as_f64()
        .unwrap_or_else(|| panic!("{stem}.{field} missing from scorer output: {scores}"))
}

/// The reported bug: the same creature, the same corpus, three directory sizes.
#[test]
fn a_creature_scores_identically_alone_and_in_a_batch() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    write_corpus(&data_dir, NUM_INPUTS, RECORDS);

    let alone = score(&creature_dir(tmp.path(), "one", 1), &data_dir, &[]);
    let pair = score(&creature_dir(tmp.path(), "two", 2), &data_dir, &[]);
    let trio = score(&creature_dir(tmp.path(), "three", 3), &data_dir, &[]);

    let solo_error = field(&alone, "creature-00", "error");
    let solo_score = field(&alone, "creature-00", "score");
    for (label, batch) in [("2 creatures", &pair), ("3 creatures", &trio)] {
        assert_eq!(
            solo_error,
            field(batch, "creature-00", "error"),
            "creature-00's error moved when scored beside {label}"
        );
        assert_eq!(
            solo_score,
            field(batch, "creature-00", "score"),
            "creature-00's score moved when scored beside {label}"
        );
        assert_eq!(
            alone["creature-00"]["recordCount"], batch["creature-00"]["recordCount"],
            "creature-00 must be scored over the whole corpus beside {label}"
        );
    }

    // The second creature of the pair is likewise unmoved by the third joining.
    assert_eq!(
        field(&pair, "creature-01", "error"),
        field(&trio, "creature-01", "error"),
        "creature-01's error moved when a third creature joined the call"
    );
}

/// The worker budget is a host knob — how many threads a machine happens to
/// have must not change what a creature scores, or two hosts disagree about the
/// same creature on the same corpus.
#[test]
fn a_creature_scores_identically_across_activation_thread_counts() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    write_corpus(&data_dir, NUM_INPUTS, RECORDS);
    let dir = creature_dir(tmp.path(), "pool", 2);

    let reference = score(&dir, &data_dir, &[("NEAT_SCORER_ACTIVATION_THREADS", "1")]);
    for threads in ["2", "5", "13"] {
        let got = score(
            &dir,
            &data_dir,
            &[("NEAT_SCORER_ACTIVATION_THREADS", threads)],
        );
        for stem in ["creature-00", "creature-01"] {
            assert_eq!(
                field(&reference, stem, "error"),
                field(&got, stem, "error"),
                "{stem}'s error moved at NEAT_SCORER_ACTIVATION_THREADS={threads}"
            );
        }
    }
}

/// Issue #537's granularity knob buys tail latency. With the partition fixed by
/// the corpus it is now free of numeric consequence, so the scores are
/// bit-identical rather than merely close.
#[test]
fn the_worker_split_knob_no_longer_moves_a_score() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    write_corpus(&data_dir, NUM_INPUTS, RECORDS);
    let dir = creature_dir(tmp.path(), "split", 3);

    let reference = score(&dir, &data_dir, &[("NEAT_SCORER_WORKER_SPLIT", "1")]);
    for split in ["2", "4", "8"] {
        let got = score(&dir, &data_dir, &[("NEAT_SCORER_WORKER_SPLIT", split)]);
        for stem in ["creature-00", "creature-01", "creature-02"] {
            assert_eq!(
                field(&reference, stem, "error"),
                field(&got, stem, "error"),
                "{stem}'s error moved at NEAT_SCORER_WORKER_SPLIT={split}"
            );
        }
    }
}
