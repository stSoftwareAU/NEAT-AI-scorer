//! Issue #202 — the pipelined GPU directory path (`inflight_chunks == 2`) must
//! produce byte-for-byte the same per-creature scores as the synchronous path
//! (`inflight_chunks == 1`).
//!
//! The pipelined path now hands each unpacked chunk to the GPU worker by
//! swapping a recycled buffer into the unpack slot (instead of cloning via
//! `to_vec()`). This test guards that the ownership-transfer + buffer-recycling
//! refactor did not change the streamed data: both modes are scored over a
//! multi-chunk `.bin` file and every creature's `error`/`score` must match
//! exactly.
//!
//! Skips cleanly when no GPU adapter is available so CPU-only CI still passes.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use rust_scorer::cost::CostKind;
use rust_scorer::fixture_json::dense_mlp_creature_json;
use rust_scorer::gpu::{GpuBackendLabel, GpuMode, resolve_backend, select_adapter};
use rust_scorer::multi_score::score_from_creature_dir_gpu;

const NUM_INPUTS: usize = 8;
const NUM_OUTPUTS: usize = 2;

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

#[test]
fn pipelined_matches_synchronous_scores_over_many_chunks() {
    // Skip cleanly when no GPU is available — CI runners are CPU-only.
    if resolve_backend(GpuMode::Auto)
        .map(|b| b.as_str() == "cpu-fallback")
        .unwrap_or(true)
    {
        eprintln!("skipping pipelined parity: no compatible adapter");
        return;
    }
    let ctx = match select_adapter() {
        Ok(Some(c)) if c.backend != GpuBackendLabel::CpuFallback => Arc::new(c),
        _ => {
            eprintln!("skipping pipelined parity: select_adapter returned no context");
            return;
        }
    };
    let backend = ctx.backend;

    // Force many streamed chunks so the pipelined buffer-recycling path runs
    // for real (multiple submit/recycle cycles), not just a single chunk.
    let read_bytes = (NUM_INPUTS + NUM_OUTPUTS) * 4 * 64;
    // SAFETY: set before any worker threads are spawned by the scorer.
    unsafe {
        std::env::set_var("NEAT_SCORER_READ_BYTES", read_bytes.to_string());
    }

    let root = std::env::temp_dir().join("gpu_pipelined_parity_fixture");
    let _ = std::fs::remove_dir_all(&root);
    let (creatures_dir, data_dir) = write_fixture(&root, 6, 20_000);

    let sync = score_from_creature_dir_gpu(
        &creatures_dir,
        &data_dir,
        backend,
        ctx.clone(),
        1, // synchronous
        CostKind::Mse,
    )
    .expect("synchronous GPU scoring");

    let pipelined = score_from_creature_dir_gpu(
        &creatures_dir,
        &data_dir,
        backend,
        ctx,
        2, // pipelined
        CostKind::Mse,
    )
    .expect("pipelined GPU scoring");

    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(
        sync.len(),
        pipelined.len(),
        "creature counts differ between sync and pipelined runs"
    );
    assert!(!sync.is_empty(), "expected at least one scored creature");

    for (key, sync_res) in &sync {
        let pipe_res = pipelined
            .get(key)
            .unwrap_or_else(|| panic!("creature '{key}' missing from pipelined results"));
        // Both paths stream identical records through the same kernel, so the
        // accumulated error and final score must be bit-identical.
        assert_eq!(
            sync_res.error.to_bits(),
            pipe_res.error.to_bits(),
            "creature '{key}': error differs (sync={}, pipelined={})",
            sync_res.error,
            pipe_res.error
        );
        assert_eq!(
            sync_res.score.to_bits(),
            pipe_res.score.to_bits(),
            "creature '{key}': score differs (sync={}, pipelined={})",
            sync_res.score,
            pipe_res.score
        );
        assert_eq!(
            sync_res.record_count, pipe_res.record_count,
            "creature '{key}': record_count differs"
        );
    }

    // Sanity: the pipelined path reported the pipelined inflight setting.
    let any = pipelined.values().next().expect("at least one result");
    assert_eq!(any.gpu_inflight_chunks, Some(2));
}
