//! Issue #316 — MAE hosted on the batched + scratch GPU kernels, at parity with
//! the CPU pipeline.
//!
//! `--cost MAE` now runs on the same multi-creature GPU path as MSE/RMSE: the
//! shared forward pass is unchanged and only the per-record reduction differs
//! (absolute error instead of squared error, selected by the shader
//! `Header.cost_kind`). This test locks the production ask from the issue on the
//! GPU directory path:
//!
//! 1. **GPU↔CPU MAE parity** — GPU MAE agrees with CPU MAE within the #81
//!    CPU↔GPU tolerance, so both kernels accumulate absolute error correctly.
//! 2. **MAE really diverges from MSE** — on the *same* GPU run, MAE ≠ MSE for a
//!    non-degenerate fixture, so a regression that ignored `cost_kind` (and kept
//!    accumulating squared error) would fail here.
//! 3. **Both kernels are exercised** — the fixture mixes small (private-array
//!    kernel) and large, > 256-neuron (scratch kernel) creatures, so MAE is
//!    validated on the production-shaped scratch path, not just a trivial
//!    creature.
//!
//! Skips cleanly when no GPU adapter is available so CPU-only CI still passes.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use rust_scorer::cost::CostKind;
use rust_scorer::fixture_json::dense_mlp_creature_json;
use rust_scorer::gpu::forward_mse_batched::MAX_NEURONS_PER_CREATURE;
use rust_scorer::gpu::{GpuBackendLabel, GpuMode, resolve_backend, select_adapter};
use rust_scorer::multi_score::{score_from_creature_dir, score_from_creature_dir_gpu};

const NUM_INPUTS: usize = 8;
const NUM_OUTPUTS: usize = 2;

/// Write a mixed creature directory (small private-kernel creatures + one large
/// scratch-kernel creature) and a single training bin.
fn write_fixture(root: &Path, n_records: usize) -> (std::path::PathBuf, std::path::PathBuf) {
    let creatures_dir = root.join("creatures");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    // Three small (private-array kernel) creatures.
    for c in 0..3 {
        std::fs::write(
            creatures_dir.join(format!("small-{c}.json")),
            // A forward-only 8→hidden→2 MLP with `TANH` hidden neurons — a real
            // forward pass with a non-linear squash, not a trivial identity
            // creature. Varying `hidden` straddles the 256-neuron kernel boundary
            // so both the private-array and scratch kernels are exercised.
            dense_mlp_creature_json(NUM_INPUTS, NUM_OUTPUTS, 8, "TANH"),
        )
        .expect("write small creature");
    }
    // One large creature above the 256-neuron cap → scratch kernel. 8 inputs +
    // 300 hidden + 2 outputs = 310 neurons.
    let large_hidden =
        (MAX_NEURONS_PER_CREATURE as usize + 50).saturating_sub(NUM_INPUTS + NUM_OUTPUTS);
    std::fs::write(
        creatures_dir.join("large-0.json"),
        dense_mlp_creature_json(NUM_INPUTS, NUM_OUTPUTS, large_hidden, "TANH"),
    )
    .expect("write large creature");

    let mut bytes = Vec::with_capacity(n_records * (NUM_INPUTS + NUM_OUTPUTS) * 4);
    for i in 0..n_records {
        for k in 0..(NUM_INPUTS + NUM_OUTPUTS) {
            // Non-trivial targets so the per-record error is clearly non-zero
            // and squared vs absolute reductions are distinguishable.
            let v = 0.5 * ((i.wrapping_mul(31) + k) as f32 * 1.7e-3).sin();
            bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    let mut f = std::fs::File::create(data_dir.join("0.bin")).expect("create bin");
    f.write_all(&bytes).expect("write bin");
    f.flush().expect("flush bin");

    (creatures_dir, data_dir)
}

#[test]
fn gpu_mae_matches_cpu_mae_and_diverges_from_mse() {
    // Skip cleanly when no GPU is available — CI runners are CPU-only.
    if resolve_backend(GpuMode::Auto)
        .map(|b| b.as_str() == "cpu-fallback")
        .unwrap_or(true)
    {
        eprintln!("skipping GPU MAE parity: no compatible adapter");
        return;
    }
    let ctx = match select_adapter() {
        Ok(Some(c)) if c.backend != GpuBackendLabel::CpuFallback => Arc::new(c),
        _ => {
            eprintln!("skipping GPU MAE parity: select_adapter returned no context");
            return;
        }
    };
    let backend = ctx.backend;

    let root = std::env::temp_dir().join("gpu_mae_parity_fixture");
    let _ = std::fs::remove_dir_all(&root);
    let (creatures_dir, data_dir) = write_fixture(&root, 8_192);

    // GPU: score the same directory under both MAE and MSE. The forward pass is
    // identical; only the per-record reduction differs.
    let gpu_mae = score_from_creature_dir_gpu(
        &creatures_dir,
        &data_dir,
        backend,
        ctx.clone(),
        1,
        CostKind::Mae,
    )
    .expect("GPU MAE scoring");
    let gpu_mse =
        score_from_creature_dir_gpu(&creatures_dir, &data_dir, backend, ctx, 1, CostKind::Mse)
            .expect("GPU MSE scoring");

    // CPU MAE baseline for the cross-backend parity assertion.
    let cpu_mae = score_from_creature_dir(
        &creatures_dir,
        &data_dir,
        GpuBackendLabel::CpuFallback,
        CostKind::Mae,
    )
    .expect("CPU MAE scoring");

    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(gpu_mae.len(), 4, "expected all four creatures scored");
    assert_eq!(gpu_mae.len(), gpu_mse.len());
    assert_eq!(gpu_mae.len(), cpu_mae.len());

    // The mixed pool must have driven the scratch kernel under MAE — otherwise
    // the large-creature absolute-error path is untested.
    let ran_scratch = gpu_mae.values().any(|r| {
        r.gpu_kernel
            .as_deref()
            .is_some_and(|k| k.contains("scratch"))
    });
    assert!(
        ran_scratch,
        "expected the scratch kernel to run for the > 256-neuron creature under MAE"
    );

    for (key, mae_res) in &gpu_mae {
        // costName must round-trip as MAE and the run must be on the GPU.
        assert_eq!(
            mae_res.cost_name, "MAE",
            "creature '{key}': costName must be MAE, got {}",
            mae_res.cost_name
        );
        assert_ne!(
            mae_res.gpu_backend,
            GpuBackendLabel::CpuFallback,
            "creature '{key}': MAE must run on the GPU, not fall back to CPU"
        );

        // (1) GPU MAE agrees with CPU MAE within the #81 CPU↔GPU tolerance.
        let cpu_res = cpu_mae
            .get(key)
            .unwrap_or_else(|| panic!("creature '{key}' missing from CPU MAE run"));
        let denom = cpu_res.error.abs().max(1e-12);
        let rel = (cpu_res.error - mae_res.error).abs() / denom;
        assert!(
            rel < 1e-3,
            "creature '{key}': CPU MAE={} GPU MAE={} relative_error={rel} exceeds 1e-3",
            cpu_res.error,
            mae_res.error
        );

        // (2) MAE genuinely differs from MSE on the same GPU run — proves the
        // kernel honoured `cost_kind` instead of always squaring the error.
        let mse_res = gpu_mse
            .get(key)
            .unwrap_or_else(|| panic!("creature '{key}' missing from GPU MSE run"));
        assert!(
            mae_res.error > 0.0,
            "creature '{key}': MAE must be positive on this fixture"
        );
        assert!(
            (mae_res.error - mse_res.error).abs() > 1e-6,
            "creature '{key}': MAE {} must diverge from MSE {} (absolute vs squared error)",
            mae_res.error,
            mse_res.error
        );
    }
}
