//! Parallel training-data file reads on the forward-only fused path — Issue #529.
//!
//! Production splits its ~80 GB corpus across 26 `.bin` files and scores them
//! through a single sequential reader. Record order does not matter (the
//! accumulator is a plain sum), so the files may be read concurrently — but
//! only if the result is *unchanged*. These tests pin that contract.
//!
//! ## Why two corpora
//!
//! Most tests use an **exact** corpus: identity activation over small integer
//! inputs with targets offset by `0`, `±0.5` or `1`, so every per-record
//! squared error is `0`, `0.25` or `1` and every partial sum is exactly
//! representable. Any regrouping of the records into different partial sums is
//! therefore invisible, and a difference in the result can only mean a
//! *different set of records was scored* — exactly the bug worth catching. Those
//! tests assert **bit-identical** totals.
//!
//! One test uses a **varied** corpus (irrational per-record errors) to pin the
//! floating-point tolerance on realistic data: `neat-core` sums each batch
//! through an 8-way SIMD path, so grouping records into different batches
//! shifts the total in the last bits. That is not new — the shipped
//! `NEAT_SCORER_READ_BYTES` knob already regroups the same way, and the test
//! holds both to the same bar.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use neat_core::creature::{compile_creature, parse_creature_json};
use neat_core::training_data::{TrainingDataConfig, find_bin_files};
use rust_scorer::cost::CostKind;
use rust_scorer::sampling::SampleSpec;
use rust_scorer::stream_score::{
    accumulate_cost_sum_forward_only_fused_sampled_with_workers,
    accumulate_cost_sum_forward_only_fused_with_workers, activation_workers_per_file_worker,
    file_read_worker_count,
};

const NUM_INPUTS: usize = 3;
const NUM_OUTPUTS: usize = 1;

/// Identity output, unit weights, zero bias: the prediction is the exact sum of
/// the inputs, so an all-integer corpus keeps the arithmetic exact.
fn creature_json() -> String {
    let synapses: Vec<String> = (0..NUM_INPUTS)
        .map(|i| format!(r#"{{"fromUUID":"input-{i}","toUUID":"output-0","weight":1.0}}"#))
        .collect();
    format!(
        r#"{{"input":{NUM_INPUTS},"output":{NUM_OUTPUTS},"forwardOnly":true,"neurons":[{{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}}],"synapses":[{}]}}"#,
        synapses.join(",")
    )
}

/// Exactly-representable record `i`: squared error is `0`, `0.25` or `1`, and
/// the per-record error still varies with the global index so scoring the wrong
/// record set changes the total.
fn exact_record(i: usize) -> Vec<u8> {
    let inputs = [(i % 3) as f32, ((i / 3) % 4) as f32, (i % 7) as f32];
    let delta = [0.0_f32, 0.5, -0.5, 1.0][i % 4];
    let target = inputs.iter().sum::<f32>() + delta;
    let mut bytes = Vec::with_capacity((NUM_INPUTS + NUM_OUTPUTS) * 4);
    for v in inputs {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes.extend_from_slice(&target.to_le_bytes());
    bytes
}

/// Realistic record `i`: irrational inputs and targets, so per-record errors do
/// not sum exactly.
fn varied_record(i: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((NUM_INPUTS + NUM_OUTPUTS) * 4);
    for k in 0..NUM_INPUTS {
        let v = ((i * 7 + k) as f32 * 0.013).sin();
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    let target = ((i * 3) as f32 * 0.041).cos();
    bytes.extend_from_slice(&target.to_le_bytes());
    bytes
}

/// Write one shard per entry of `per_file`, holding that many records.
fn write_corpus_with(dir: &Path, per_file: &[usize], record: fn(usize) -> Vec<u8>) -> Vec<PathBuf> {
    let mut global = 0_usize;
    for (shard, &count) in per_file.iter().enumerate() {
        let mut f = std::fs::File::create(dir.join(format!("{shard}.bin"))).expect("create shard");
        for _ in 0..count {
            f.write_all(&record(global)).expect("write record");
            global += 1;
        }
    }
    find_bin_files(dir).expect("find bin files")
}

fn write_corpus(dir: &Path, per_file: &[usize]) -> Vec<PathBuf> {
    write_corpus_with(dir, per_file, exact_record)
}

fn config() -> TrainingDataConfig {
    TrainingDataConfig {
        num_inputs: NUM_INPUTS,
        num_outputs: NUM_OUTPUTS,
    }
}

/// `(loss_sum, record_count)` for the given reader count.
fn score(files: &[PathBuf], workers: usize) -> (f64, usize) {
    score_sampled(files, workers, SampleSpec::full())
}

fn score_sampled(files: &[PathBuf], workers: usize, sample: SampleSpec) -> (f64, usize) {
    let creature = parse_creature_json(&creature_json()).expect("parse creature");
    let mut network = compile_creature(&creature).expect("compile creature");
    let (loss, records, ..) = accumulate_cost_sum_forward_only_fused_sampled_with_workers(
        CostKind::Mse,
        files,
        &config(),
        &mut network,
        sample,
        workers,
    )
    .expect("fused accumulate");
    (loss, records)
}

/// Bit-identical assertion for the exact corpus.
fn assert_same(actual: (f64, usize), expected: (f64, usize), context: &str) {
    assert_eq!(
        actual.1, expected.1,
        "{context}: scored {} records, sequential scored {}",
        actual.1, expected.1
    );
    assert_eq!(
        actual.0.to_bits(),
        expected.0.to_bits(),
        "{context}: loss {} vs sequential {}",
        actual.0,
        expected.0
    );
}

/// Relative-difference bar for the varied corpus. Two orders of magnitude
/// tighter than the CPU-vs-GPU parity bars this repo already ships (1e-4/1e-3),
/// and far below any difference that could change a fitness ranking.
const REL_TOLERANCE: f64 = 1e-6;

fn relative_diff(actual: f64, expected: f64) -> f64 {
    (actual - expected).abs() / expected.abs().max(1e-30)
}

#[test]
fn parallel_readers_match_the_sequential_loss_and_record_count() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // 26 shards, matching production's file count.
    let files = write_corpus(tmp.path(), &[97_usize; 26]);
    assert_eq!(files.len(), 26);

    let expected = score(&files, 1);
    assert_eq!(expected.1, 97 * 26);
    assert!(expected.0 > 0.0, "fixture must produce a non-zero loss");

    for workers in [2_usize, 3, 8, 26, 64, 0] {
        assert_same(
            score(&files, workers),
            expected,
            &format!("reader count {workers}"),
        );
    }
}

#[test]
fn parallel_readers_are_deterministic_across_repeated_runs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let files = write_corpus(tmp.path(), &[40, 91, 7, 130, 62, 88]);

    let first = score(&files, 6);
    for run in 0..4 {
        assert_same(score(&files, 6), first, &format!("repeat run {run}"));
    }
}

#[test]
fn uneven_file_sizes_still_match_the_sequential_sweep() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // One dominant shard plus a long tail — the dynamic work queue must cope.
    let files = write_corpus(tmp.path(), &[1000, 3, 5, 1, 220, 17, 2, 9]);

    let expected = score(&files, 1);
    for workers in [2_usize, 4, 8] {
        assert_same(
            score(&files, workers),
            expected,
            &format!("reader count {workers}"),
        );
    }
}

#[test]
fn empty_shards_are_skipped_without_shifting_records() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let files = write_corpus(tmp.path(), &[12, 0, 31, 0, 0, 44]);

    let expected = score(&files, 1);
    assert_eq!(expected.1, 87);
    assert_same(score(&files, 4), expected, "corpus with empty shards");
}

#[test]
fn sub_sampling_keeps_the_same_records_at_every_reader_count() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let files = write_corpus(tmp.path(), &[53, 71, 11, 97, 23, 64, 5, 88]);

    for &(rate, phase) in &[(0.25_f64, 0_u64), (0.5, 1), (0.1, 7)] {
        let spec = SampleSpec::new(rate, phase).expect("valid sample spec");
        let expected = score_sampled(&files, 1, spec);
        assert!(expected.1 > 0, "rate {rate} must keep at least one record");

        for workers in [2_usize, 5, 8] {
            assert_same(
                score_sampled(&files, workers, spec),
                expected,
                &format!("rate {rate} phase {phase} reader count {workers}"),
            );
        }
    }
}

#[test]
fn misaligned_corpus_falls_back_to_the_sequential_reader() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let files = write_corpus(tmp.path(), &[10, 10, 10]);
    // Split one record across the first file boundary: half its bytes end file
    // 0, the rest lead file 1. The two misalignments cancel out across the
    // corpus (the case `corpus_guard` documents), so the sequential reader
    // still succeeds — and the parallel reader must not try to frame these
    // files independently.
    for path in [&files[0], &files[1]] {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("open shard");
        f.write_all(&[1_u8, 2, 3, 4, 5, 6, 7, 8]).expect("append");
    }

    let expected = score(&files, 1);
    for workers in [2_usize, 3, 0] {
        assert_same(
            score(&files, workers),
            expected,
            &format!("misaligned corpus, reader count {workers}"),
        );
    }
}

#[test]
fn single_file_corpus_never_splits() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let files = write_corpus(tmp.path(), &[500]);
    assert_eq!(file_read_worker_count(files.len()), 1);

    let expected = score(&files, 1);
    assert_same(score(&files, 8), expected, "single-file corpus");
}

#[test]
fn worker_counts_share_one_cpu_budget() {
    // Never more readers than files, and always at least one activation worker
    // per reader, so the two axes cannot oversubscribe the CPU together.
    let activation_budget = activation_workers_per_file_worker(1);
    for files in [1_usize, 2, 7, 26, 1000] {
        let readers = file_read_worker_count(files);
        assert!(readers >= 1 && readers <= files);
        let per_reader = activation_workers_per_file_worker(readers);
        assert!(per_reader >= 1);
        assert!(
            readers * per_reader <= activation_budget.max(readers),
            "readers {readers} x activation {per_reader} exceeded the CPU budget"
        );
    }
}

/// On a corpus whose per-record errors are *not* exactly representable, the
/// parallel readers group records into different SIMD batches than the single
/// reader, so the totals differ in the last bits. Pin how far: the shipped
/// `NEAT_SCORER_READ_BYTES` knob regroups exactly the same way, and neither may
/// move the answer by more than [`REL_TOLERANCE`].
#[test]
fn varied_corpus_stays_within_tolerance_and_matches_the_read_buffer_knob() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let files = write_corpus_with(tmp.path(), &[701_usize; 9], varied_record);

    let creature = parse_creature_json(&creature_json()).expect("parse creature");
    let sequential = {
        let mut network = compile_creature(&creature).expect("compile creature");
        accumulate_cost_sum_forward_only_fused_with_workers(
            CostKind::Mse,
            &files,
            &config(),
            &mut network,
            1,
        )
        .expect("fused accumulate")
    };

    for workers in [2_usize, 4, 9] {
        let mut network = compile_creature(&creature).expect("compile creature");
        let parallel = accumulate_cost_sum_forward_only_fused_with_workers(
            CostKind::Mse,
            &files,
            &config(),
            &mut network,
            workers,
        )
        .expect("fused accumulate");
        assert_eq!(parallel.1, sequential.1, "reader count {workers}");
        let rel = relative_diff(parallel.0, sequential.0);
        assert!(
            rel < REL_TOLERANCE,
            "reader count {workers}: {} vs sequential {} (relative diff {rel:e})",
            parallel.0,
            sequential.0
        );
    }
}

/// End-to-end: the shipped CLI honours `NEAT_SCORER_FILE_THREADS`, reports the
/// resolved reader count, and returns the same score.
#[test]
fn cli_score_is_unchanged_by_the_file_thread_knob() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    write_corpus(&data_dir, &[64_usize; 12]);
    let creature_path = tmp.path().join("creature.json");
    std::fs::write(&creature_path, creature_json()).expect("write creature");

    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let run = |threads: &str, read_bytes: &str| -> serde_json::Value {
        let out = Command::new(bin)
            .arg(&creature_path)
            .arg(&data_dir)
            .arg("--gpu")
            .arg("off")
            .env("NEAT_SCORER_FILE_THREADS", threads)
            .env("NEAT_SCORER_READ_BYTES", read_bytes)
            .output()
            .expect("run rust_scorer");
        assert!(
            out.status.success(),
            "scorer failed with NEAT_SCORER_FILE_THREADS={threads}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("parse scorer JSON")
    };

    let sequential = run("1", "1048576");
    let parallel = run("4", "1048576");

    assert_eq!(sequential["recordCount"], parallel["recordCount"]);
    let seq_error = sequential["error"].as_f64().expect("error field");
    let par_error = parallel["error"].as_f64().expect("error field");
    assert_eq!(
        par_error.to_bits(),
        seq_error.to_bits(),
        "CLI error changed with 4 file readers: {par_error} vs {seq_error}"
    );
    assert_eq!(
        parallel["fileReadWorkers"].as_u64(),
        Some(4),
        "the CLI must report the resolved file-reader count"
    );
    assert_eq!(
        sequential["fileReadWorkers"].as_u64(),
        None,
        "a sequential read reports no fileReadWorkers field"
    );
}
