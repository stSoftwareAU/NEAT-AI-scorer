//! Issue #42 / #355: permanent automated assertion that multi-creature
//! (directory/batch) scoring compiles each creature **exactly once**, cloning
//! the resulting `CompiledNetwork` for any additional workers — independent of
//! how many workers the pool allocates.
//!
//! This is the behavioural (WHAT) replacement for the machine-dependent
//! wall-clock threshold (`compileTimeSecs < 1.0`) that previously guarded the
//! same regression in `tests/directory_mode_tdd.rs`. Wall-clock budgets flake
//! on saturated CI runners and were toothless here anyway: 32 sub-millisecond
//! recompiles of tiny fixtures still land far under 1.0 s. This test instead
//! resets a compile probe, runs one scoring invocation, and asserts the
//! observed compile count equals the creature count `N`. The pre-#42
//! regression compiled once per (creature × worker); on any multi-core host
//! that yields far more than `N` compiles, so the `assert_eq!` below fails
//! before the regression can land.

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use rust_scorer::cost::CostKind;
use rust_scorer::gpu::GpuBackendLabel;
use rust_scorer::multi_score::{compile_probe, score_from_creature_dir};

/// The compile probe is a process-global counter, so tests in this binary that
/// touch it must not run concurrently. Serialise them through this lock;
/// recover from poisoning so one failing test does not cascade into the others.
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

/// Write a training corpus of `records` (input, output) pairs into a single
/// `.bin` file.
fn write_training_data(data_dir: &Path, records: &[(f32, f32)]) {
    let mut file = std::fs::File::create(data_dir.join("0.bin")).expect("create bin");
    for (input, output) in records {
        file.write_all(&input.to_le_bytes()).expect("write input");
        file.write_all(&output.to_le_bytes()).expect("write output");
    }
}

/// Score `n_creatures` against a fixed corpus on the CPU path and return the
/// number of `compile_creature` calls observed for that single invocation.
fn compiles_for_batch(n_creatures: usize) -> u64 {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let creatures_dir = tmp.path().join("creatures");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir(&data_dir).expect("create data dir");

    write_creatures(&creatures_dir, n_creatures);
    let records: Vec<(f32, f32)> = (0..64).map(|i| (i as f32 * 0.1, i as f32 * 0.1)).collect();
    write_training_data(&data_dir, &records);

    compile_probe::reset();
    let scores = score_from_creature_dir(
        &creatures_dir,
        &data_dir,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
    )
    .expect("scoring must succeed");

    // Sanity: the run really scored every creature in the batch.
    assert_eq!(
        scores.len(),
        n_creatures,
        "expected a score for each of the {n_creatures} creatures",
    );

    compile_probe::count()
}

/// Core assertion: scoring N creatures compiles exactly N times, regardless of
/// how many workers the pool allocates. The pre-#42 per-worker recompile would
/// report `N × workers` compiles on any multi-core host and fail here.
#[test]
fn multi_creature_batch_compiles_each_creature_once() {
    let _guard = PROBE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    for &n in &[2_usize, 5, 11] {
        let compiles = compiles_for_batch(n);
        assert_eq!(
            compiles, n as u64,
            "scoring {n} creatures must compile exactly {n} times, observed {compiles}",
        );
    }
}

/// The single-creature path also compiles exactly once, pinning that N == 1 and
/// N > 1 share the same compile-once contract.
#[test]
fn single_creature_compiles_once() {
    let _guard = PROBE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let compiles = compiles_for_batch(1);
    assert_eq!(
        compiles, 1,
        "scoring a single creature must compile exactly once, observed {compiles}",
    );
}
