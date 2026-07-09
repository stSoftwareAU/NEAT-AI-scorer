//! Issue #310 — record-level `--sample-rate` in the forward-only streaming
//! reader, exercised through the production directory-mode batch entrypoint
//! ([`score_from_creature_dir_sampled`]).
//!
//! Production spends ~95 % of its fitness wall-clock in the forward-only Rust
//! batch path over a single large corpus file, where shard-level selection
//! cannot help. These tests drive the real library entrypoint against a temp
//! corpus and assert that a sub-unity rate scores exactly the stratified
//! subsample (half the records at rate 0.5), continuous across streamed chunks,
//! while rate 1 reproduces the full-corpus result bit-for-bit.

use std::io::Write;
use std::path::{Path, PathBuf};

use rust_scorer::cost::CostKind;
use rust_scorer::gpu::GpuBackendLabel;
use rust_scorer::multi_score::{score_from_creature_dir, score_from_creature_dir_sampled};
use rust_scorer::sample::SampleSpec;

const INPUTS: usize = 1;
const OUTPUTS: usize = 1;
const RECORD_BYTES: usize = (INPUTS + OUTPUTS) * 4;

/// Forward-only identity creature: output = weight * input.
fn identity_creature() -> String {
    format!(
        r#"{{"input":{INPUTS},"output":{OUTPUTS},"forwardOnly":true,"neurons":[{{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}}],"synapses":[{{"fromUUID":"input-0","toUUID":"output-0","weight":1.0}}]}}"#
    )
}

/// Write `n_records` records where record `r` has input `r` and target `0`, so
/// the identity creature's per-record squared error is `r*r`.
fn write_training_data(dir: &Path, n_records: usize) {
    std::fs::create_dir_all(dir).expect("create data dir");
    let mut file = std::fs::File::create(dir.join("0.bin")).expect("create data file");
    for r in 0..n_records {
        for v in [r as f32, 0.0] {
            file.write_all(&v.to_le_bytes()).expect("write f32");
        }
    }
}

/// Force many small streamed chunks so the subsample stride must stay
/// continuous across chunk boundaries (not restart per chunk).
fn force_small_reads() {
    // SAFETY: set before the scorer spawns worker threads.
    unsafe {
        std::env::set_var("NEAT_SCORER_READ_BYTES", RECORD_BYTES.to_string());
    }
}

fn fixture(tag: &str, n_records: usize) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("sample_rate_dir_{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let creatures = root.join("creatures");
    let data = root.join("data");
    std::fs::create_dir_all(&creatures).expect("create creatures dir");
    std::fs::write(creatures.join("creature-0.json"), identity_creature()).expect("write creature");
    write_training_data(&data, n_records);
    (creatures, data)
}

/// Rate 0.5 keeps odd record indices continuously across many small chunks, so
/// the reported `record_count` is exactly half and the mean error is computed
/// over only the kept records.
#[test]
fn half_rate_scores_stratified_subsample() {
    force_small_reads();
    let n = 8_usize;
    let (creatures, data) = fixture("half", n);

    let full = score_from_creature_dir(
        &creatures,
        &data,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
    )
    .expect("full score");
    let sampled = score_from_creature_dir_sampled(
        &creatures,
        &data,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
        SampleSpec::new(0.5, 0).unwrap(),
    )
    .expect("sampled score");

    let full_r = full.get("creature-0").expect("full result");
    let sampled_r = sampled.get("creature-0").expect("sampled result");

    assert_eq!(full_r.record_count, n);
    assert_eq!(sampled_r.record_count, n / 2);

    // Kept records are indices 1,3,5,7 → mean squared error (1+9+25+49)/4 = 21.
    let want_sampled = (1.0 + 9.0 + 25.0 + 49.0) / 4.0;
    assert!(
        (sampled_r.error - want_sampled).abs() < 1e-4,
        "sampled error {} != {want_sampled}",
        sampled_r.error
    );
    // Full mean squared error over 0..7 = 140/8 = 17.5.
    assert!(
        (full_r.error - 17.5).abs() < 1e-4,
        "full error {} != 17.5",
        full_r.error
    );
}

/// Rate 1 must reproduce the full-corpus directory result bit-for-bit — the
/// default path never perturbs historical scoring (Issue #310).
#[test]
fn rate_one_matches_full_corpus() {
    force_small_reads();
    let (creatures, data) = fixture("rate_one", 20);

    let full = score_from_creature_dir(
        &creatures,
        &data,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
    )
    .expect("full score");
    let rate_one = score_from_creature_dir_sampled(
        &creatures,
        &data,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
        SampleSpec::new(1.0, 0).unwrap(),
    )
    .expect("rate-one score");

    let full_r = full.get("creature-0").expect("full result");
    let one_r = rate_one.get("creature-0").expect("rate-one result");
    assert_eq!(full_r.record_count, one_r.record_count);
    assert!((full_r.error - one_r.error).abs() < 1e-12);
    assert!((full_r.score - one_r.score).abs() < 1e-12);
}
