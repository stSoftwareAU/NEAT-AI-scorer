//! Issue #308 — early-exit / partial-score API for directory-mode batch scoring.
//!
//! These integration tests drive the real library entrypoints
//! ([`score_from_creature_dir`] and
//! [`score_from_creature_dir_with_early_exit`]) against a temp corpus and
//! assert on the returned [`ScoreResult`]s — the full-score parity contract,
//! per-creature abort freezing its partial score, and `AbortAll` stopping the
//! sweep early.
//!
//! A tiny `NEAT_SCORER_READ_BYTES` forces many streamed chunks so the callback
//! fires repeatedly and an abort genuinely removes work from later chunks
//! (rather than the whole corpus arriving in a single chunk).

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};

use rust_scorer::cost::CostKind;
use rust_scorer::gpu::GpuBackendLabel;
use rust_scorer::multi_score::{
    EarlyExit, PartialScore, score_from_creature_dir, score_from_creature_dir_with_early_exit,
};

const INPUTS: usize = 2;
const OUTPUTS: usize = 1;
const RECORD_BYTES: usize = (INPUTS + OUTPUTS) * 4;

/// Forward-only creature mapping `w0*x0 + w1*x1` onto a single IDENTITY output.
/// Distinct weights give each creature a distinct error so abort selection is
/// meaningful.
fn linear_creature(w0: f32, w1: f32) -> String {
    format!(
        r#"{{"input":{INPUTS},"output":{OUTPUTS},"forwardOnly":true,"neurons":[{{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}}],"synapses":[{{"fromUUID":"input-0","toUUID":"output-0","weight":{w0}}},{{"fromUUID":"input-1","toUUID":"output-0","weight":{w1}}}]}}"#
    )
}

/// Materialise `n` creatures with linearly-varying weights into `dir`.
fn write_creatures(dir: &Path, n: usize) {
    std::fs::create_dir_all(dir).expect("create creatures dir");
    for i in 0..n {
        let w0 = 0.3 + 0.15 * i as f32;
        let w1 = 0.5;
        std::fs::write(
            dir.join(format!("creature-{i:03}.json")),
            linear_creature(w0, w1),
        )
        .expect("write creature");
    }
}

/// Write `n_records` deterministic records to a single `.bin` file.
fn write_training_data(dir: &Path, n_records: usize) {
    std::fs::create_dir_all(dir).expect("create data dir");
    let mut file = std::fs::File::create(dir.join("0.bin")).expect("create data file");
    for r in 0..n_records {
        let x0 = (r as f32 * 0.01).sin();
        let x1 = (r as f32 * 0.017).cos();
        let target = 0.4 * x0 + 0.5 * x1; // ground truth close to, but not, any creature
        for v in [x0, x1, target] {
            file.write_all(&v.to_le_bytes()).expect("write f32");
        }
    }
}

/// Force many small streamed chunks so the early-exit callback fires often.
fn force_small_reads() {
    // SAFETY: set before the scorer spawns any worker threads; every test in
    // this binary sets the same value, so concurrent tests do not observe a
    // conflicting size.
    unsafe {
        std::env::set_var("NEAT_SCORER_READ_BYTES", RECORD_BYTES.to_string());
    }
}

fn fixture(tag: &str, n_creatures: usize, n_records: usize) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("early_exit_tdd_{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let creatures = root.join("creatures");
    let data = root.join("data");
    write_creatures(&creatures, n_creatures);
    write_training_data(&data, n_records);
    (creatures, data)
}

/// Full-score parity: a callback that always returns `Continue` must produce
/// results bit-identical to the plain `score_from_creature_dir`.
#[test]
fn always_continue_matches_full_score_baseline() {
    force_small_reads();
    let (creatures, data) = fixture("parity", 6, 400);

    let baseline = score_from_creature_dir(
        &creatures,
        &data,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
    )
    .expect("baseline score");

    let calls = RefCell::new(0usize);
    let early = score_from_creature_dir_with_early_exit(
        &creatures,
        &data,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
        |partials| {
            *calls.borrow_mut() += 1;
            // Every creature stays active for the whole corpus.
            assert_eq!(partials.len(), 6);
            EarlyExit::Continue
        },
    )
    .expect("early-exit score");

    assert!(
        *calls.borrow() > 1,
        "callback should fire once per chunk (many chunks)"
    );
    assert_eq!(baseline.len(), early.len());
    for (key, base) in &baseline {
        let got = early.get(key).expect("same keys");
        assert_eq!(
            base.error.to_bits(),
            got.error.to_bits(),
            "error for {key} must be bit-identical on the full-score path"
        );
        assert_eq!(base.score.to_bits(), got.score.to_bits(), "score for {key}");
        assert_eq!(
            base.record_count, got.record_count,
            "record_count for {key}"
        );
    }
}

/// Aborting a subset of creatures freezes their partial score (record_count <
/// full) while the survivors match the full-score baseline exactly.
#[test]
fn abort_creatures_freezes_partial_and_keeps_survivors_exact() {
    force_small_reads();
    let (creatures, data) = fixture("abort_subset", 6, 400);

    let baseline = score_from_creature_dir(
        &creatures,
        &data,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
    )
    .expect("baseline score");
    let full_records = baseline.values().next().unwrap().record_count;

    // Abort creatures 0, 2, 4 on the first callback; survivors 1, 3, 5.
    let aborted_once = RefCell::new(false);
    let early = score_from_creature_dir_with_early_exit(
        &creatures,
        &data,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
        |partials| {
            if !*aborted_once.borrow() {
                *aborted_once.borrow_mut() = true;
                let losers: Vec<usize> = partials
                    .iter()
                    .map(|p| p.creature_index)
                    .filter(|i| i % 2 == 0)
                    .collect();
                return EarlyExit::AbortCreatures(losers);
            }
            // After the first abort, only survivors remain active.
            assert!(partials.iter().all(|p| p.creature_index % 2 == 1));
            EarlyExit::Continue
        },
    )
    .expect("early-exit score");

    for (key, res) in &early {
        let idx: usize = key.trim_start_matches("creature-").parse().unwrap();
        if idx % 2 == 0 {
            // Aborted: scored fewer records than the full corpus.
            assert!(
                res.record_count < full_records && res.record_count > 0,
                "aborted {key} record_count {} should be in (0, {full_records})",
                res.record_count
            );
        } else {
            // Survivor: identical to the full-score baseline.
            let base = &baseline[key];
            assert_eq!(res.record_count, full_records, "survivor {key} full corpus");
            assert_eq!(
                base.error.to_bits(),
                res.error.to_bits(),
                "survivor {key} error must match baseline exactly"
            );
        }
    }
}

/// `AbortAll` stops the sweep immediately: every creature freezes with a
/// partial (non-zero, below-full) record count and still produces a valid score.
#[test]
fn abort_all_stops_sweep_early_for_every_creature() {
    force_small_reads();
    let (creatures, data) = fixture("abort_all", 4, 400);

    let baseline = score_from_creature_dir(
        &creatures,
        &data,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
    )
    .expect("baseline score");
    let full_records = baseline.values().next().unwrap().record_count;

    let seen = RefCell::new(0usize);
    let early = score_from_creature_dir_with_early_exit(
        &creatures,
        &data,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
        |_partials| {
            *seen.borrow_mut() += 1;
            EarlyExit::AbortAll
        },
    )
    .expect("early-exit score");

    // AbortAll on the very first chunk ⇒ callback fires exactly once.
    assert_eq!(*seen.borrow(), 1);
    assert_eq!(early.len(), 4);
    for (key, res) in &early {
        assert!(
            res.record_count > 0 && res.record_count < full_records,
            "{key} should have a partial record_count, got {}",
            res.record_count
        );
        assert!(res.score.is_finite(), "{key} score must be finite");
    }
}

/// The partial-score snapshot exposes the running mean error and a
/// monotonically-growing `records_scored` per creature.
#[test]
fn partial_score_snapshot_is_well_formed() {
    force_small_reads();
    let (creatures, data) = fixture("snapshot", 3, 300);

    let last_records: RefCell<Vec<usize>> = RefCell::new(vec![0; 3]);
    let observed: RefCell<Vec<PartialScore>> = RefCell::new(Vec::new());
    let _ = score_from_creature_dir_with_early_exit(
        &creatures,
        &data,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
        |partials| {
            for p in partials {
                assert!(p.records_scored >= 1);
                assert!(p.partial_error.is_finite() && p.partial_error >= 0.0);
                assert_eq!(p.key, format!("creature-{:03}", p.creature_index));
                let mut last = last_records.borrow_mut();
                assert!(
                    p.records_scored >= last[p.creature_index],
                    "records_scored must be monotonic"
                );
                last[p.creature_index] = p.records_scored;
            }
            observed.borrow_mut().extend(partials.iter().cloned());
            EarlyExit::Continue
        },
    )
    .expect("early-exit score");

    assert!(!observed.borrow().is_empty(), "callback must have fired");
}
