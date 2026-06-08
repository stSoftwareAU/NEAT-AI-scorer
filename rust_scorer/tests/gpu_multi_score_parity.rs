//! Issue #82 — CPU vs GPU per-creature MSE parity for the batched kernel.
//!
//! When a real wgpu adapter is available, runs the GPU forward+MSE shader on
//! the same synthetic 8→8→2 fixture used by the bench harness for N=10 and
//! N=50 creatures and asserts each creature's MSE sum agrees with the CPU
//! `mse_sum_batch_packed` path within `1e-4` relative tolerance. Skips the
//! body when no adapter is present so CI runners (which do not expose a GPU)
//! still pass.

use std::sync::Arc;

use neat_core::creature::{compile_creature, parse_creature_json};
use neat_core::loss::mse_sum_batch_packed;

use rust_scorer::gpu::forward_mse_batched::{BatchedRunner, KernelKind, MAX_NEURONS_PER_CREATURE};
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

fn build_test_records(num_inputs: usize, num_outputs: usize, n_records: usize) -> Vec<f32> {
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

fn run_parity(
    num_creatures: usize,
    num_inputs: usize,
    num_outputs: usize,
    hidden: usize,
    n_records: usize,
) {
    // Skip cleanly when no GPU is available — CI runners are CPU-only.
    if resolve_backend(GpuMode::Auto)
        .map(|b| b.as_str() == "cpu-fallback")
        .unwrap_or(true)
    {
        eprintln!("skipping GPU parity: no compatible adapter");
        return;
    }
    let ctx = match select_adapter() {
        Ok(Some(c)) => Arc::new(c),
        _ => {
            eprintln!("skipping GPU parity: select_adapter returned no context");
            return;
        }
    };

    let json = synthetic_creature_json(num_inputs, num_outputs, hidden);
    let creature = parse_creature_json(&json).expect("parse creature");
    let template = compile_creature(&creature).expect("compile");
    let mut nets: Vec<_> = (0..num_creatures).map(|_| template.clone()).collect();

    let records = build_test_records(num_inputs, num_outputs, n_records);

    // CPU baseline — per-creature MSE sum.
    let cpu_sums: Vec<f64> = nets
        .iter_mut()
        .map(|net| mse_sum_batch_packed(net, &records, num_inputs, num_outputs, true))
        .collect();

    // GPU: build the runner and dispatch a single chunk.
    let mut runner = BatchedRunner::new(ctx, &nets, num_inputs, num_outputs)
        .expect("supported squash types in synthetic fixture");

    // The runner must route to the kernel matching the creature size: the fast
    // private-array kernel at or below the 256 cap, the scratch kernel above it.
    let total_neurons = (num_inputs + hidden + num_outputs) as u32;
    let expected = if total_neurons > MAX_NEURONS_PER_CREATURE {
        KernelKind::Scratch
    } else {
        KernelKind::Private
    };
    assert_eq!(
        runner.kernel(),
        expected,
        "creature with {total_neurons} neurons routed to the wrong kernel",
    );

    let gpu_sums = runner.score_chunk(&records, n_records);

    assert_eq!(cpu_sums.len(), gpu_sums.len());
    for (i, (cpu, gpu)) in cpu_sums.iter().zip(gpu_sums.iter()).enumerate() {
        let denom = cpu.abs().max(1e-12);
        let rel = (cpu - gpu).abs() / denom;
        assert!(
            rel < 1e-4,
            "creature {i}: CPU={cpu} GPU={gpu} relative_error={rel} exceeds 1e-4",
        );
    }

    // Diagnostic counter is incremented on each chunk dispatch.
    assert!(runner.dispatch_count >= 1);
}

#[test]
fn cpu_vs_gpu_n10_creatures_within_relative_tolerance() {
    run_parity(10, 8, 2, 8, 4096);
}

#[test]
fn cpu_vs_gpu_n50_creatures_within_relative_tolerance() {
    run_parity(50, 8, 2, 8, 4096);
}

#[test]
fn cpu_vs_gpu_handles_partial_workgroup_remainder() {
    // Pick `n_records` that is not a multiple of WG_SIZE_X (64) so the shader's
    // bounds check on the trailing partial workgroup is exercised.
    run_parity(10, 8, 2, 8, 100);
}

// Issue #182 — large creatures (above the 256 private-array cap) must score on
// the `forward_mse_scratch` kernel with results matching the CPU path.

#[test]
fn cpu_vs_gpu_large_creature_just_above_cap() {
    // 8 + 300 + 2 = 310 neurons > 256 cap → scratch kernel.
    run_parity(8, 8, 2, 300, 4096);
}

#[test]
fn cpu_vs_gpu_production_scale_4000_neuron_creature() {
    // 8 + 4000 + 2 = 4010 neurons — the production scale the issue targets.
    // Smaller record count keeps the test within the unit-test time budget.
    run_parity(4, 8, 2, 4000, 512);
}

#[test]
fn cpu_vs_gpu_large_creature_grid_stride_remainder() {
    // Records not a multiple of WG_SIZE_X exercise the grid-stride remainder on
    // the scratch path as well.
    run_parity(6, 8, 2, 300, 300);
}
