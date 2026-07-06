//! Issue #3236: permanent automated assertion that multi-creature
//! (directory/batch) scoring makes **exactly one pass** over the training data,
//! scoring the whole batch in that single sweep — independent of the number of
//! creatures.
//!
//! The assertion is deterministic: `multi_score` records every sweep over the
//! training corpus through
//! [`rust_scorer::multi_score::training_pass_probe`], so these tests reset the
//! probe, run one scoring invocation, then assert the observed sweep count is
//! `1`. If a future change reintroduces per-creature re-reads of the training
//! data (looping the `for_each_read_chunk` sweep once per creature), the sweep
//! count would equal the creature count `N` and the `assert_eq!` below fails —
//! breaking `cargo test` before the regression can land.

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use rust_scorer::cost::CostKind;
use rust_scorer::gpu::GpuBackendLabel;
use rust_scorer::multi_score::{score_from_creature_dir, training_pass_probe};

/// The sweep probe is a process-global counter, so tests in this binary that
/// touch it must not run concurrently. Serialise them through this lock; recover
/// from poisoning so one failing test does not cascade into the others.
static PROBE_LOCK: Mutex<()> = Mutex::new(());

/// A minimal forward-only identity creature (1 input, 1 output). Every creature
/// in a directory-mode batch must share the same input/output shape.
fn minimal_creature() -> &'static str {
    r#"{"input":1,"output":1,"forwardOnly":true,"neurons":[{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}],"synapses":[{"fromUUID":"input-0","toUUID":"output-0","weight":1.0}]}"#
}

/// Write `n` distinct creature JSON files (identical shape, distinct filenames)
/// into `creatures_dir`.
fn write_creatures(creatures_dir: &Path, n: usize) {
    for i in 0..n {
        std::fs::write(
            creatures_dir.join(format!("creature-{i}.json")),
            minimal_creature(),
        )
        .expect("write creature json");
    }
}

/// Write a training corpus of `records` (input, output) pairs across the given
/// number of `.bin` files, round-robin, so multi-file traversal is exercised.
fn write_training_data(data_dir: &Path, records: &[(f32, f32)], bin_files: usize) {
    let files: Vec<std::fs::File> = (0..bin_files)
        .map(|i| std::fs::File::create(data_dir.join(format!("{i}.bin"))).expect("create bin"))
        .collect();
    let mut files = files;
    for (idx, (input, output)) in records.iter().enumerate() {
        let f = &mut files[idx % bin_files];
        f.write_all(&input.to_le_bytes()).expect("write input");
        f.write_all(&output.to_le_bytes()).expect("write output");
    }
}

/// Score `n_creatures` against a fixed corpus on the CPU path and return the
/// number of training-data sweeps observed for that single invocation.
fn sweeps_for_batch(n_creatures: usize, bin_files: usize) -> u64 {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let creatures_dir = tmp.path().join("creatures");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir(&data_dir).expect("create data dir");

    write_creatures(&creatures_dir, n_creatures);
    let records: Vec<(f32, f32)> = (0..64).map(|i| (i as f32 * 0.1, i as f32 * 0.1)).collect();
    write_training_data(&data_dir, &records, bin_files);

    training_pass_probe::reset();
    let scores = score_from_creature_dir(
        &creatures_dir,
        &data_dir,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
    )
    .expect("scoring must succeed");

    // Sanity: the single sweep really scored every creature in the batch.
    assert_eq!(
        scores.len(),
        n_creatures,
        "expected a score for each of the {n_creatures} creatures",
    );

    training_pass_probe::count()
}

/// Core assertion: scoring N > 1 creatures traverses the training data exactly
/// once, regardless of N. A regression that re-reads per creature would report
/// `N` sweeps instead of `1`.
#[test]
fn multi_creature_batch_makes_exactly_one_pass() {
    let _guard = PROBE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    for &n in &[2_usize, 5, 11] {
        let sweeps = sweeps_for_batch(n, 1);
        assert_eq!(
            sweeps, 1,
            "scoring {n} creatures must make exactly one pass over the training data, observed {sweeps}",
        );
    }
}

/// The one-pass property is independent of how many `.bin` files the corpus is
/// split across: a single sweep still covers the whole multi-file corpus.
#[test]
fn multi_creature_batch_one_pass_across_multiple_bin_files() {
    let _guard = PROBE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let sweeps = sweeps_for_batch(4, 3);
    assert_eq!(
        sweeps, 1,
        "one sweep must cover the whole multi-file corpus, observed {sweeps}",
    );
}

/// Contrast case (Scope item 3): the single-creature path also makes exactly
/// one pass, pinning that N == 1 and N > 1 share the same one-sweep contract.
#[test]
fn single_creature_makes_exactly_one_pass() {
    let _guard = PROBE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let sweeps = sweeps_for_batch(1, 1);
    assert_eq!(
        sweeps, 1,
        "scoring a single creature must make exactly one pass, observed {sweeps}",
    );
}
