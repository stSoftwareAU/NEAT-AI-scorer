//! Issue #319 — pipelined GPU directory scoring (`inflight_chunks == 2`) with the
//! scratch kernel and production-scale read buffers must complete multi-`.bin` corpora
//! without Metal SIGSEGV. The scorer clamps scratch/mixed pools to synchronous
//! dispatches; this test guards parity across that boundary.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use rust_scorer::cost::CostKind;
use rust_scorer::gpu::forward_mse_batched::DirectoryGpuTopology;
use rust_scorer::gpu::forward_mse_batched::effective_directory_gpu_inflight;
use rust_scorer::gpu::{GpuBackendLabel, GpuMode, resolve_backend, select_adapter};
use rust_scorer::multi_score::score_from_creature_dir_gpu;

const NUM_INPUTS: usize = 8;
const NUM_OUTPUTS: usize = 2;
const HIDDEN: usize = 300;

fn synthetic_creature_json() -> String {
    let mut neurons: Vec<String> = Vec::new();
    for h in 0..HIDDEN {
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
        for h in 0..HIDDEN {
            let w = 0.05 + 0.001 * ((i * HIDDEN + h) as f64);
            synapses.push(format!(
                r#"{{"fromUUID":"input-{i}","toUUID":"hidden-{h}","weight":{w}}}"#
            ));
        }
    }
    for h in 0..HIDDEN {
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

fn write_bin(path: &Path, n_records: usize) {
    let mut bytes = Vec::with_capacity(n_records * (NUM_INPUTS + NUM_OUTPUTS) * 4);
    for i in 0..n_records {
        for k in 0..(NUM_INPUTS + NUM_OUTPUTS) {
            let v = ((i.wrapping_mul(31) + k) as f32 * 1.0e-3).sin();
            bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    let mut f = std::fs::File::create(path).expect("create bin");
    f.write_all(&bytes).expect("write bin");
    f.flush().expect("flush bin");
}

fn write_fixture(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let creatures_dir = root.join("creatures");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    std::fs::write(
        creatures_dir.join("scratch.json"),
        synthetic_creature_json(),
    )
    .expect("write creature");

    // Mirror the A-2007 / A-2008 record counts that triggered the Metal crash
    // with 32 MiB reads — multiple chunks per file plus a file boundary.
    write_bin(&data_dir.join("shard-0.bin"), 7_118);
    write_bin(&data_dir.join("shard-1.bin"), 7_182);

    (creatures_dir, data_dir)
}

#[test]
fn scratch_topology_clamps_pipelined_inflight() {
    assert_eq!(
        effective_directory_gpu_inflight(DirectoryGpuTopology::ScratchOnly, 2),
        1
    );
}

#[test]
fn scratch_multi_bin_pipelined_matches_synchronous() {
    if resolve_backend(GpuMode::Auto)
        .map(|b| b.as_str() == "cpu-fallback")
        .unwrap_or(true)
    {
        eprintln!("skipping scratch multi-bin pipelined parity: no compatible adapter");
        return;
    }
    let ctx = match select_adapter() {
        Ok(Some(c)) if c.backend != GpuBackendLabel::CpuFallback => Arc::new(c),
        _ => {
            eprintln!("skipping scratch multi-bin pipelined parity: no GPU context");
            return;
        }
    };
    let backend = ctx.backend;

    // Enough bytes per read to span multiple chunks per shard and a file
    // boundary, without allocating an ~8M-float unpack buffer (small test
    // creatures use 40 B/record vs production's ~9848 B).
    let record_bytes = (NUM_INPUTS + NUM_OUTPUTS) * 4;
    let read_bytes = record_bytes * 64 * 64;
    unsafe {
        std::env::set_var("NEAT_SCORER_READ_BYTES", read_bytes.to_string());
        std::env::set_var(
            "NEAT_SCORER_GPU_SCRATCH_BYTES",
            (512 * 1024 * 1024).to_string(),
        );
    }

    let root = std::env::temp_dir().join("gpu_pipelined_scratch_multi_bin");
    let _ = std::fs::remove_dir_all(&root);
    let (creatures_dir, data_dir) = write_fixture(&root);

    let sync = score_from_creature_dir_gpu(
        &creatures_dir,
        &data_dir,
        backend,
        ctx.clone(),
        1,
        CostKind::Mse,
    )
    .expect("synchronous scratch multi-bin GPU scoring");

    let pipelined =
        score_from_creature_dir_gpu(&creatures_dir, &data_dir, backend, ctx, 2, CostKind::Mse)
            .expect("pipelined scratch multi-bin GPU scoring (clamped to sync)");

    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(sync.len(), pipelined.len());
    let sync_res = sync.get("scratch").expect("sync creature");
    let pipe_res = pipelined.get("scratch").expect("pipelined creature");
    assert_eq!(sync_res.error.to_bits(), pipe_res.error.to_bits());
    assert_eq!(sync_res.score.to_bits(), pipe_res.score.to_bits());
    assert_eq!(sync_res.record_count, pipe_res.record_count);
    assert_eq!(
        pipe_res.gpu_inflight_chunks,
        Some(1),
        "scratch pool must report synchronous inflight after Issue #319 clamp"
    );
}
