//! Issue #339 — RMSE served on the existing GPU MSE kernel via a host-side
//! `sqrt`.
//!
//! `--cost RMSE` reuses `forward_mse_batched` unchanged (the kernel returns the
//! squared-error sum RMSE needs); only the creature-directory finalisation in
//! `multi_score.rs` differs, taking `sqrt(mean)` instead of `mean`. This test
//! locks two contracts on the GPU directory path:
//!
//! 1. **GPU↔CPU RMSE parity** — GPU RMSE agrees with CPU RMSE within the #81
//!    CPU↔GPU tolerance, so the shared MSE kernel serves RMSE correctly.
//! 2. **The `sqrt` finalisation is really applied** — RMSE equals the square
//!    root of the MSE score on the *same* GPU run. A missing `sqrt` (a
//!    regression that stopped routing line ~1233 through `finalise_mean`) would
//!    report MSE-scale numbers under `costName: "RMSE"` and fail here.
//!
//! Skips cleanly when no GPU adapter is available so CPU-only CI still passes.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use rust_scorer::cost::CostKind;
use rust_scorer::gpu::{GpuBackendLabel, GpuMode, resolve_backend, select_adapter};
use rust_scorer::multi_score::{score_from_creature_dir, score_from_creature_dir_gpu};

const NUM_INPUTS: usize = 8;
const NUM_OUTPUTS: usize = 2;

/// Small all-private synthetic creature (8 + 8 + 2 = 18 neurons, well under the
/// 256-neuron shader cap) so the GPU path routes to `forward_mse_batched`.
fn synthetic_creature_json(hidden: usize) -> String {
    let mut neurons: Vec<String> = Vec::new();
    for h in 0..hidden {
        neurons.push(format!(
            r#"{{"type":"hidden","uuid":"hidden-{h}","bias":0.05,"squash":"TANH"}}"#
        ));
    }
    for o in 0..NUM_OUTPUTS {
        neurons.push(format!(
            r#"{{"type":"output","uuid":"output-{o}","bias":0.0,"squash":"IDENTITY"}}"#
        ));
    }
    let mut synapses: Vec<String> = Vec::new();
    for i in 0..NUM_INPUTS {
        for h in 0..hidden {
            let w = 0.05 + 0.001 * ((i * hidden + h) as f64);
            synapses.push(format!(
                r#"{{"fromUUID":"input-{i}","toUUID":"hidden-{h}","weight":{w}}}"#
            ));
        }
    }
    for h in 0..hidden {
        for o in 0..NUM_OUTPUTS {
            let w = 0.1 + 0.001 * ((h * NUM_OUTPUTS + o) as f64);
            synapses.push(format!(
                r#"{{"fromUUID":"hidden-{h}","toUUID":"output-{o}","weight":{w}}}"#
            ));
        }
    }
    format!(
        r#"{{"input":{NUM_INPUTS},"output":{NUM_OUTPUTS},"forwardOnly":true,"semanticVersion":"4.0.0","neurons":[{}],"synapses":[{}]}}"#,
        neurons.join(","),
        synapses.join(","),
    )
}

fn write_fixture(
    root: &Path,
    num_creatures: usize,
    n_records: usize,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let creatures_dir = root.join("creatures");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    let json = synthetic_creature_json(8);
    for c in 0..num_creatures {
        std::fs::write(creatures_dir.join(format!("creature-{c}.json")), &json)
            .expect("write creature");
    }

    let mut bytes = Vec::with_capacity(n_records * (NUM_INPUTS + NUM_OUTPUTS) * 4);
    for i in 0..n_records {
        for k in 0..(NUM_INPUTS + NUM_OUTPUTS) {
            // Non-trivial targets so the squared-error sum (and thus RMSE) is
            // clearly non-zero and the sqrt is observable.
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
fn gpu_rmse_matches_cpu_rmse_and_is_sqrt_of_mse() {
    // Skip cleanly when no GPU is available — CI runners are CPU-only.
    if resolve_backend(GpuMode::Auto)
        .map(|b| b.as_str() == "cpu-fallback")
        .unwrap_or(true)
    {
        eprintln!("skipping GPU RMSE parity: no compatible adapter");
        return;
    }
    let ctx = match select_adapter() {
        Ok(Some(c)) if c.backend != GpuBackendLabel::CpuFallback => Arc::new(c),
        _ => {
            eprintln!("skipping GPU RMSE parity: select_adapter returned no context");
            return;
        }
    };
    let backend = ctx.backend;

    let root = std::env::temp_dir().join("gpu_rmse_parity_fixture");
    let _ = std::fs::remove_dir_all(&root);
    let (creatures_dir, data_dir) = write_fixture(&root, 4, 8_192);

    // GPU: score the same directory under both MSE and RMSE. The kernel is
    // identical; only the host-side finalisation differs.
    let gpu_mse = score_from_creature_dir_gpu(
        &creatures_dir,
        &data_dir,
        backend,
        ctx.clone(),
        1,
        CostKind::Mse,
    )
    .expect("GPU MSE scoring");
    let gpu_rmse =
        score_from_creature_dir_gpu(&creatures_dir, &data_dir, backend, ctx, 1, CostKind::Rmse)
            .expect("GPU RMSE scoring");

    // CPU RMSE baseline for the cross-backend parity assertion.
    let cpu_rmse = score_from_creature_dir(
        &creatures_dir,
        &data_dir,
        GpuBackendLabel::CpuFallback,
        CostKind::Rmse,
    )
    .expect("CPU RMSE scoring");

    let _ = std::fs::remove_dir_all(&root);

    assert!(
        !gpu_rmse.is_empty(),
        "expected at least one scored creature"
    );
    assert_eq!(gpu_rmse.len(), gpu_mse.len());
    assert_eq!(gpu_rmse.len(), cpu_rmse.len());

    for (key, rmse_res) in &gpu_rmse {
        // costName must round-trip as RMSE.
        assert_eq!(
            rmse_res.cost_name, "RMSE",
            "creature '{key}': costName must be RMSE, got {}",
            rmse_res.cost_name
        );

        // (2) The host-side sqrt is really applied: RMSE == sqrt(MSE mean) on
        // the same GPU run. `error` is the finalised mean loss.
        let mse_res = gpu_mse
            .get(key)
            .unwrap_or_else(|| panic!("creature '{key}' missing from GPU MSE run"));
        let expected_rmse = mse_res.error.sqrt();
        assert!(
            (rmse_res.error - expected_rmse).abs() <= 1e-9 * expected_rmse.max(1.0),
            "creature '{key}': GPU RMSE {} must equal sqrt(GPU MSE {}) = {}",
            rmse_res.error,
            mse_res.error,
            expected_rmse
        );
        assert!(
            rmse_res.error > 0.0,
            "creature '{key}': RMSE must be positive (a missing sqrt would still be positive, \
             but a zero here means the fixture is degenerate)"
        );

        // (1) GPU RMSE agrees with CPU RMSE within the #81 CPU↔GPU tolerance.
        let cpu_res = cpu_rmse
            .get(key)
            .unwrap_or_else(|| panic!("creature '{key}' missing from CPU RMSE run"));
        let denom = cpu_res.error.abs().max(1e-12);
        let rel = (cpu_res.error - rmse_res.error).abs() / denom;
        assert!(
            rel < 1e-3,
            "creature '{key}': CPU RMSE={} GPU RMSE={} relative_error={rel} exceeds 1e-3",
            cpu_res.error,
            rmse_res.error
        );
    }
}
