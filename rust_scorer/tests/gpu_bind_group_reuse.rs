//! Issue #322 — the batched GPU runner reuses its bind group across dispatches.
//!
//! `create_bind_group` was called on every `score_chunk`; the bind group only
//! becomes stale when a growable buffer is reallocated or the scratch binding is
//! resized. These tests assert that steady-state, same-size chunks reuse a single
//! bind group (one build for many dispatches) and that a larger chunk — which
//! grows the records/partials buffers — forces exactly one rebuild, all while the
//! GPU results still match the CPU baseline.
//!
//! Skips cleanly when no GPU adapter is present so CPU-only CI still passes.

use std::sync::Arc;

use neat_core::creature::{compile_creature, parse_creature_json};
use neat_core::loss::mse_sum_batch_packed;

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

#[test]
fn same_size_chunks_reuse_one_bind_group() {
    let Some(ctx) = gpu_ctx() else { return };

    let (num_inputs, num_outputs, hidden) = (8, 2, 8);
    let json = synthetic_creature_json(num_inputs, num_outputs, hidden);
    let template = compile_creature(&parse_creature_json(&json).expect("parse")).expect("compile");
    let nets: Vec<_> = (0..4).map(|_| template.clone()).collect();

    let mut runner = BatchedRunner::new(ctx, &nets, num_inputs, num_outputs)
        .expect("synthetic fixture is GPU-supported");

    let n_records = 256;
    let records = build_records(num_inputs, num_outputs, n_records);

    // Five dispatches of identical shape.
    for _ in 0..5 {
        runner
            .score_chunk(&records, n_records)
            .expect("GPU readback should succeed");
    }

    assert_eq!(runner.dispatch_count, 5, "five chunks were dispatched");
    assert_eq!(
        runner.bind_group_builds, 1,
        "identical-shape chunks must reuse a single bind group (built once)",
    );
}

#[test]
fn growing_chunk_forces_exactly_one_rebuild() {
    let Some(ctx) = gpu_ctx() else { return };

    let (num_inputs, num_outputs, hidden) = (8, 2, 8);
    let json = synthetic_creature_json(num_inputs, num_outputs, hidden);
    let template = compile_creature(&parse_creature_json(&json).expect("parse")).expect("compile");
    let nets: Vec<_> = (0..4).map(|_| template.clone()).collect();

    let mut runner = BatchedRunner::new(ctx, &nets, num_inputs, num_outputs)
        .expect("synthetic fixture is GPU-supported");

    // Two small same-size chunks: one build, reused once.
    let small = build_records(num_inputs, num_outputs, 256);
    runner.score_chunk(&small, 256).expect("small chunk 1");
    runner.score_chunk(&small, 256).expect("small chunk 2");
    assert_eq!(runner.bind_group_builds, 1, "small chunks share one group");

    // A much larger chunk grows the records/partials buffers → one rebuild.
    let large = build_records(num_inputs, num_outputs, 4096);
    runner.score_chunk(&large, 4096).expect("large chunk");
    assert_eq!(
        runner.bind_group_builds, 2,
        "growing the buffers forces exactly one bind-group rebuild",
    );

    // Returning to the small shape reuses the (now larger) buffers without a
    // further rebuild — the records buffer capacity already covers it.
    runner.score_chunk(&small, 256).expect("small chunk 3");
    assert_eq!(
        runner.bind_group_builds, 2,
        "shrinking back to a covered shape must not rebuild the bind group",
    );
}

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

    let mut runner = BatchedRunner::new(ctx, &nets, num_inputs, num_outputs)
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
    assert_eq!(
        runner.bind_group_builds, 1,
        "the reused bind group served all three parity dispatches",
    );
}
