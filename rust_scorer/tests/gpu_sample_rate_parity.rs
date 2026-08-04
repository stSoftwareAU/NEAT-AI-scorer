//! Issue #358 — direct coverage for the GPU directory-scoring path with
//! record-level sub-sampling (`score_from_creature_dir_gpu_sampled`,
//! `multi_score.rs`), the production CLI's `--sample-rate` + GPU combination.
//!
//! The existing suite left this exact combination untested: the GPU parity
//! tests (`gpu_pipelined_parity`, `gpu_multi_score_parity`) only drive the
//! full-rate wrapper, while every `--sample-rate` test runs with `--gpu off`.
//! A refactor of the sampling plumbing on the GPU path could therefore silently
//! change production scores with nothing to catch it.
//!
//! These behaviour-based (WHAT) tests assert observable results only — never
//! internals:
//!   * a sub-unity rate on the GPU matches the already-tested CPU sampled path
//!     (`score_from_creature_dir_sampled`) within GPU float tolerance, and keeps
//!     exactly the stratified subsample count; and
//!   * `SampleSpec` at rate 1.0 reproduces the full-corpus GPU result
//!     bit-for-bit (the documented full-rate contract).
//!
//! Both skip cleanly when no GPU adapter is available so CPU-only CI passes.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use rust_scorer::cost::CostKind;
use rust_scorer::fixture_json::dense_mlp_creature_json;
use rust_scorer::gpu::{GpuBackendLabel, GpuContext, GpuMode, resolve_backend, select_adapter};
use rust_scorer::multi_score::{
    score_from_creature_dir_gpu, score_from_creature_dir_gpu_sampled,
    score_from_creature_dir_sampled,
};
use rust_scorer::sampling::SampleSpec;

const NUM_INPUTS: usize = 8;
const NUM_OUTPUTS: usize = 2;

/// Write a multi-creature directory plus a corpus whose records carry a
/// deterministic per-record pattern, so a sub-rate stride keeps a distinct
/// subset (not a constant repeated record).
fn write_fixture(
    root: &Path,
    num_creatures: usize,
    n_records: usize,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let creatures_dir = root.join("creatures");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    let json = dense_mlp_creature_json(NUM_INPUTS, NUM_OUTPUTS, 8, "TANH");
    for c in 0..num_creatures {
        std::fs::write(creatures_dir.join(format!("creature-{c}.json")), &json)
            .expect("write creature");
    }

    let mut bytes = Vec::with_capacity(n_records * (NUM_INPUTS + NUM_OUTPUTS) * 4);
    for i in 0..n_records {
        for k in 0..(NUM_INPUTS + NUM_OUTPUTS) {
            let v = ((i.wrapping_mul(31) + k) as f32 * 1.0e-3).sin();
            bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    let mut f = std::fs::File::create(data_dir.join("0.bin")).expect("create bin");
    f.write_all(&bytes).expect("write bin");
    f.flush().expect("flush bin");

    (creatures_dir, data_dir)
}

/// Acquire a real GPU context, or `None` (with a skip note) when CI is CPU-only.
fn gpu_ctx(tag: &str) -> Option<Arc<GpuContext>> {
    if resolve_backend(GpuMode::Auto)
        .map(|b| b.as_str() == "cpu-fallback")
        .unwrap_or(true)
    {
        eprintln!("skipping {tag}: no compatible adapter");
        return None;
    }
    match select_adapter() {
        Ok(Some(c)) if c.backend != GpuBackendLabel::CpuFallback => Some(Arc::new(c)),
        _ => {
            eprintln!("skipping {tag}: select_adapter returned no context");
            None
        }
    }
}

/// Rate 0.5 on the GPU path must score exactly the same stratified subsample as
/// the already-tested CPU sampled path, within GPU float tolerance. This is the
/// combination unique to `score_from_creature_dir_gpu_sampled` and previously
/// had no safety net.
#[test]
fn gpu_sampled_matches_cpu_sampled_half_rate() {
    let ctx = match gpu_ctx("gpu sampled half-rate parity") {
        Some(c) => c,
        None => return,
    };
    let backend = ctx.backend;

    let root = std::env::temp_dir().join("gpu_sample_rate_parity_half");
    let _ = std::fs::remove_dir_all(&root);
    let n_records = 4_096;
    let (creatures_dir, data_dir) = write_fixture(&root, 4, n_records);

    let sample = SampleSpec::new(0.5, 0).expect("valid sample spec");

    let cpu = score_from_creature_dir_sampled(
        &creatures_dir,
        &data_dir,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
        &sample,
    )
    .expect("CPU sampled scoring");

    let gpu = score_from_creature_dir_gpu_sampled(
        &creatures_dir,
        &data_dir,
        backend,
        ctx,
        1, // synchronous — the private kernel path
        CostKind::Mse,
        &sample,
    )
    .expect("GPU sampled scoring");

    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(
        cpu.len(),
        gpu.len(),
        "creature counts differ between CPU and GPU sampled runs"
    );
    assert!(!gpu.is_empty(), "expected at least one scored creature");

    for (key, cpu_res) in &cpu {
        let gpu_res = gpu
            .get(key)
            .unwrap_or_else(|| panic!("creature '{key}' missing from GPU sampled results"));

        // The sub-rate stride is deterministic and shared by both paths, so both
        // must keep exactly the same record count (half of 4096).
        assert_eq!(
            cpu_res.record_count,
            n_records / 2,
            "creature '{key}': CPU kept {} records, expected {}",
            cpu_res.record_count,
            n_records / 2
        );
        assert_eq!(
            gpu_res.record_count, cpu_res.record_count,
            "creature '{key}': GPU kept {} records, CPU kept {}",
            gpu_res.record_count, cpu_res.record_count
        );

        // GPU is f32 vs CPU f64 accumulation — compare within relative tolerance.
        let denom = cpu_res.error.abs().max(1e-9);
        let rel = (cpu_res.error - gpu_res.error).abs() / denom;
        assert!(
            rel < 1e-3,
            "creature '{key}': CPU error={} GPU error={} relative_error={rel} exceeds 1e-3",
            cpu_res.error,
            gpu_res.error
        );
    }
}

/// `SampleSpec` at rate 1.0 must reproduce the full-corpus GPU result
/// bit-for-bit — the documented full-rate contract that keeps the sampled path
/// a zero-cost default when no sub-sampling is requested.
#[test]
fn gpu_sampled_rate_one_matches_gpu_full() {
    let ctx = match gpu_ctx("gpu sampled rate-one parity") {
        Some(c) => c,
        None => return,
    };
    let backend = ctx.backend;

    let root = std::env::temp_dir().join("gpu_sample_rate_parity_rate_one");
    let _ = std::fs::remove_dir_all(&root);
    let (creatures_dir, data_dir) = write_fixture(&root, 3, 2_048);

    let full = score_from_creature_dir_gpu(
        &creatures_dir,
        &data_dir,
        backend,
        ctx.clone(),
        1,
        CostKind::Mse,
    )
    .expect("full-rate GPU scoring");

    let rate_one = score_from_creature_dir_gpu_sampled(
        &creatures_dir,
        &data_dir,
        backend,
        ctx,
        1,
        CostKind::Mse,
        &SampleSpec::new(1.0, 0).expect("valid sample spec"),
    )
    .expect("rate-one GPU sampled scoring");

    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(full.len(), rate_one.len(), "creature counts differ");
    assert!(!full.is_empty(), "expected at least one scored creature");

    for (key, full_res) in &full {
        let one_res = rate_one
            .get(key)
            .unwrap_or_else(|| panic!("creature '{key}' missing from rate-one results"));
        assert_eq!(
            full_res.record_count, one_res.record_count,
            "creature '{key}': record_count differs at rate 1.0"
        );
        assert_eq!(
            full_res.error.to_bits(),
            one_res.error.to_bits(),
            "creature '{key}': error differs at rate 1.0 (full={}, rate_one={})",
            full_res.error,
            one_res.error
        );
        assert_eq!(
            full_res.score.to_bits(),
            one_res.score.to_bits(),
            "creature '{key}': score differs at rate 1.0 (full={}, rate_one={})",
            full_res.score,
            one_res.score
        );
    }
}
