//! Issue #322 / #357 — the batched GPU runner reuses its bind group across
//! dispatches without changing observable results.
//!
//! Bind-group caching is a non-contractual internal optimisation (Issue #322):
//! its only observable surface is score parity (results match the CPU baseline
//! and stay stable across repeated / grown / shrunk chunks) and throughput
//! (rebuilds stay well below one-per-dispatch). These tests therefore assert
//! that observable behaviour — WHAT a caller sees — rather than pinning the
//! exact rebuild policy (Issue #357). The rebuild *decision logic* is already
//! covered by the pure `bind_group_needs_rebuild` unit tests in
//! `src/gpu/forward_mse_batched.rs`, so we keep only a loose regression guard
//! here: across many dispatches the cache must build far fewer bind groups than
//! dispatches (no per-dispatch rebuild), without asserting an exact count that a
//! benign buffer-management refactor would break.
//!
//! Skips cleanly when no GPU adapter is present so CPU-only CI still passes.

use std::sync::Arc;

use neat_core::creature::{compile_creature, parse_creature_json};
use neat_core::loss::mse_sum_batch_packed;

use rust_scorer::cost::CostKind;
use rust_scorer::gpu::forward_mse_batched::BatchedRunner;
use rust_scorer::gpu::{GpuMode, resolve_backend, select_adapter};

fn synthetic_creature_json(num_inputs: usize, num_outputs: usize, hidden: usize) -> String {
    let mut neurons: Vec<String> = Vec::new();
    for h in 0..hidden {
        neurons.push(format!(
            r#"{{"type":"hidden","uuid":"hidden-{h}","bias":0.05,"squash":"TANH"}}"#
        ));
    }
    for o in 0..num_outputs {
        neurons.push(format!(
            r#"{{"type":"output","uuid":"output-{o}","bias":0.0,"squash":"IDENTITY"}}"#
        ));
    }
    let mut synapses: Vec<String> = Vec::new();
    for i in 0..num_inputs {
        for h in 0..hidden {
            let w = 0.05 + 0.001 * ((i * hidden + h) as f64);
            synapses.push(format!(
                r#"{{"fromUUID":"input-{i}","toUUID":"hidden-{h}","weight":{w}}}"#
            ));
        }
    }
    for h in 0..hidden {
        for o in 0..num_outputs {
            let w = 0.1 + 0.001 * ((h * num_outputs + o) as f64);
            synapses.push(format!(
                r#"{{"fromUUID":"hidden-{h}","toUUID":"output-{o}","weight":{w}}}"#
            ));
        }
    }
    format!(
        r#"{{"input":{num_inputs},"output":{num_outputs},"forwardOnly":true,"semanticVersion":"4.0.0","neurons":[{}],"synapses":[{}]}}"#,
        neurons.join(","),
        synapses.join(","),
    )
}

fn build_records(num_inputs: usize, num_outputs: usize, n_records: usize) -> Vec<f32> {
    let values_per_record = num_inputs + num_outputs;
    let mut floats = Vec::with_capacity(n_records * values_per_record);
    for i in 0..n_records {
        for k in 0..values_per_record {
            let v = ((i.wrapping_mul(31) + k) as f32 * 1.0e-3).sin();
            floats.push(v);
        }
    }
    floats
}

/// Acquire a GPU context or `None` when no compatible adapter is present.
fn gpu_ctx() -> Option<Arc<rust_scorer::gpu::GpuContext>> {
    if resolve_backend(GpuMode::Auto)
        .map(|b| b.as_str() == "cpu-fallback")
        .unwrap_or(true)
    {
        eprintln!("skipping bind-group reuse test: no compatible adapter");
        return None;
    }
    match select_adapter() {
        Ok(Some(c)) => Some(Arc::new(c)),
        _ => {
            eprintln!("skipping bind-group reuse test: select_adapter returned no context");
            None
        }
    }
}

/// Identical-shape chunks are deterministic (same input → same sums) and the
/// bind-group cache does not rebuild on every dispatch — an observable
/// throughput property that survives any benign change to the caching policy.
#[test]
fn same_size_chunks_are_deterministic_and_reuse_bind_groups() {
    let Some(ctx) = gpu_ctx() else { return };

    let (num_inputs, num_outputs, hidden) = (8, 2, 8);
    let json = synthetic_creature_json(num_inputs, num_outputs, hidden);
    let template = compile_creature(&parse_creature_json(&json).expect("parse")).expect("compile");
    let nets: Vec<_> = (0..4).map(|_| template.clone()).collect();

    let mut runner = BatchedRunner::new(ctx, &nets, num_inputs, num_outputs, CostKind::Mse)
        .expect("synthetic fixture is GPU-supported");

    let n_records = 256;
    let records = build_records(num_inputs, num_outputs, n_records);

    // Five dispatches of identical shape must all return the same sums.
    let first = runner
        .score_chunk(&records, n_records)
        .expect("GPU readback should succeed");
    for _ in 0..4 {
        let again = runner
            .score_chunk(&records, n_records)
            .expect("GPU readback should succeed");
        assert_eq!(
            again, first,
            "identical-shape chunks must produce identical sums on every dispatch",
        );
    }

    assert_eq!(runner.dispatch_count, 5, "five chunks were dispatched");
    // Loose optimisation guard (Issue #357): reuse means far fewer builds than
    // dispatches — never the per-dispatch rebuild the cache exists to avoid. The
    // exact build count is a non-contractual policy detail and is not asserted.
    assert!(
        runner.bind_group_builds < runner.dispatch_count,
        "steady-state dispatches must reuse bind groups (builds {} < dispatches {})",
        runner.bind_group_builds,
        runner.dispatch_count,
    );
}

/// Growing then shrinking the chunk size is transparent to results: the buffers
/// grow to fit the large chunk and the later small chunk reuses that capacity,
/// yet every dispatch of the same shape returns the same sums. Buffer
/// management stays a loose optimisation — no exact rebuild count is pinned.
#[test]
fn grown_and_shrunk_chunks_stay_correct() {
    let Some(ctx) = gpu_ctx() else { return };

    let (num_inputs, num_outputs, hidden) = (8, 2, 8);
    let json = synthetic_creature_json(num_inputs, num_outputs, hidden);
    let template = compile_creature(&parse_creature_json(&json).expect("parse")).expect("compile");
    let nets: Vec<_> = (0..4).map(|_| template.clone()).collect();

    let mut runner = BatchedRunner::new(ctx, &nets, num_inputs, num_outputs, CostKind::Mse)
        .expect("synthetic fixture is GPU-supported");

    let small = build_records(num_inputs, num_outputs, 256);
    let large = build_records(num_inputs, num_outputs, 4096);

    // Small chunk, grow to a large chunk, then shrink back to the small shape.
    let small_before = runner.score_chunk(&small, 256).expect("small chunk 1");
    let large_first = runner.score_chunk(&large, 4096).expect("large chunk 1");
    let small_after = runner.score_chunk(&small, 256).expect("small chunk 2");
    let large_again = runner.score_chunk(&large, 4096).expect("large chunk 2");

    // Buffer growth and shrink-back must not change the scored result for a
    // given shape — that is the only behaviour a caller can observe.
    assert_eq!(
        small_before, small_after,
        "the small chunk scores identically before and after the buffers grew",
    );
    assert_eq!(
        large_first, large_again,
        "the large chunk scores identically across a shrink-and-regrow",
    );

    assert_eq!(runner.dispatch_count, 4, "four chunks were dispatched");
    // Loose optimisation guard (Issue #357): the cache reuses bind groups often
    // enough that builds stay below the dispatch count even with growth. The
    // exact number of rebuilds on growth/shrink is a non-contractual policy
    // detail and is deliberately not asserted.
    assert!(
        runner.bind_group_builds < runner.dispatch_count,
        "bind-group reuse must outpace rebuilds (builds {} < dispatches {})",
        runner.bind_group_builds,
        runner.dispatch_count,
    );
}

/// The reused bind group never drifts from the CPU baseline across repeated
/// dispatches — the core WHAT-assertion of the optimisation.
#[test]
fn reused_bind_group_preserves_cpu_parity() {
    let Some(ctx) = gpu_ctx() else { return };

    let (num_inputs, num_outputs, hidden) = (8, 2, 8);
    let json = synthetic_creature_json(num_inputs, num_outputs, hidden);
    let template = compile_creature(&parse_creature_json(&json).expect("parse")).expect("compile");
    let mut nets: Vec<_> = (0..4).map(|_| template.clone()).collect();

    let n_records = 512;
    let records = build_records(num_inputs, num_outputs, n_records);
    let cpu_sums: Vec<f64> = nets
        .iter_mut()
        .map(|net| mse_sum_batch_packed(net, &records, num_inputs, num_outputs, true))
        .collect();

    let mut runner = BatchedRunner::new(ctx, &nets, num_inputs, num_outputs, CostKind::Mse)
        .expect("synthetic fixture is GPU-supported");

    // Dispatch the same chunk three times; the 2nd and 3rd reuse the bind group.
    // Every dispatch must still agree with the CPU baseline.
    for pass in 0..3 {
        let gpu_sums = runner
            .score_chunk(&records, n_records)
            .expect("GPU readback should succeed");
        assert_eq!(cpu_sums.len(), gpu_sums.len());
        for (i, (cpu, gpu)) in cpu_sums.iter().zip(gpu_sums.iter()).enumerate() {
            let denom = cpu.abs().max(1e-12);
            let rel = (cpu - gpu).abs() / denom;
            assert!(
                rel < 1e-4,
                "pass {pass} creature {i}: CPU={cpu} GPU={gpu} rel={rel} exceeds 1e-4",
            );
        }
    }
    // Loose optimisation guard (Issue #357): three identical dispatches must not
    // rebuild the bind group each time. The exact build count is a
    // non-contractual policy detail and is not asserted.
    assert!(
        runner.bind_group_builds < runner.dispatch_count,
        "identical dispatches reuse the bind group (builds {} < dispatches {})",
        runner.bind_group_builds,
        runner.dispatch_count,
    );
}
