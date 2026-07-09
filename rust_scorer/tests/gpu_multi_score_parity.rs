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
    synthetic_creature_json_squash(num_inputs, num_outputs, hidden, "TANH")
}

/// Like [`synthetic_creature_json`] but with an explicit hidden-layer squash so
/// the CPU↔GPU parity tests can exercise every activation the shader inlines
/// (Issue #305).
fn synthetic_creature_json_squash(
    num_inputs: usize,
    num_outputs: usize,
    hidden: usize,
    hidden_squash: &str,
) -> String {
    let mut neurons: Vec<String> = Vec::new();
    for h in 0..hidden {
        neurons.push(format!(
            r#"{{"type":"hidden","uuid":"hidden-{h}","bias":0.05,"squash":"{hidden_squash}"}}"#
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

    let gpu_sums = runner
        .score_chunk(&records, n_records)
        .expect("GPU readback should succeed in parity fixture");

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

/// Build records whose input slice spans a wider range than the default
/// `build_test_records` so each activation is exercised on both sides of zero
/// (Issue #305 parity coverage). Targets are left small so the MSE stays
/// well-conditioned.
fn build_varied_records(num_inputs: usize, num_outputs: usize, n_records: usize) -> Vec<f32> {
    let values_per_record = num_inputs + num_outputs;
    let mut floats = Vec::with_capacity(n_records * values_per_record);
    for i in 0..n_records {
        for k in 0..num_inputs {
            // Roughly [-3, 3], deterministic and varied across (record, input).
            let v = 3.0 * ((i.wrapping_mul(37) + k * 13) as f32 * 7.0e-3).sin();
            floats.push(v);
        }
        for k in 0..num_outputs {
            let v = 0.25 * ((i.wrapping_mul(19) + k) as f32 * 3.0e-3).cos();
            floats.push(v);
        }
    }
    floats
}

/// CPU↔GPU parity for a single named hidden-layer squash (Issue #305).
fn run_parity_squash(hidden_squash: &str, num_creatures: usize, hidden: usize, n_records: usize) {
    let num_inputs = 8;
    let num_outputs = 2;

    // Skip cleanly when no GPU is available — CI runners are CPU-only.
    if resolve_backend(GpuMode::Auto)
        .map(|b| b.as_str() == "cpu-fallback")
        .unwrap_or(true)
    {
        eprintln!("skipping GPU parity for {hidden_squash}: no compatible adapter");
        return;
    }
    let ctx = match select_adapter() {
        Ok(Some(c)) => Arc::new(c),
        _ => {
            eprintln!("skipping GPU parity for {hidden_squash}: no context");
            return;
        }
    };

    let json = synthetic_creature_json_squash(num_inputs, num_outputs, hidden, hidden_squash);
    let creature = parse_creature_json(&json).expect("parse creature");
    let template = compile_creature(&creature).expect("compile");
    let mut nets: Vec<_> = (0..num_creatures).map(|_| template.clone()).collect();

    let records = build_varied_records(num_inputs, num_outputs, n_records);

    let cpu_sums: Vec<f64> = nets
        .iter_mut()
        .map(|net| mse_sum_batch_packed(net, &records, num_inputs, num_outputs, true))
        .collect();

    let mut runner = BatchedRunner::new(ctx, &nets, num_inputs, num_outputs)
        .unwrap_or_else(|e| panic!("squash {hidden_squash} must be GPU-supported: {e:?}"));
    let gpu_sums = runner
        .score_chunk(&records, n_records)
        .expect("GPU readback should succeed");

    assert_eq!(cpu_sums.len(), gpu_sums.len());
    for (i, (cpu, gpu)) in cpu_sums.iter().zip(gpu_sums.iter()).enumerate() {
        let denom = cpu.abs().max(1e-9);
        let rel = (cpu - gpu).abs() / denom;
        assert!(
            rel < 1e-3,
            "squash {hidden_squash} creature {i}: CPU={cpu} GPU={gpu} relative_error={rel} exceeds 1e-3",
        );
    }
}

/// Issue #305: every point-wise squash the shader now inlines must score within
/// tolerance of the CPU path. Covers the production GRQ blockers named in the
/// issue (ABSOLUTE, BENT_IDENTITY, SELU, SINE, GELU, CUBE, HARD_TANH, …) plus
/// the rest of the 0..=31 set.
#[test]
fn cpu_vs_gpu_pointwise_squash_coverage() {
    // Names must match `neat_core::creature::parse_squash_name`.
    const SQUASHES: &[&str] = &[
        "IDENTITY",
        "RELU",
        "ReLU6",
        "LeakyReLU",
        "SELU",
        "ELU",
        "LOGISTIC",
        "TANH",
        "HARD_TANH",
        "SOFTSIGN",
        "Softplus",
        "Swish",
        "Mish",
        "GELU",
        "SINE",
        "Cosine",
        "TAN",
        "ArcTan",
        "GAUSSIAN",
        "BENT_IDENTITY",
        "BIPOLAR_SIGMOID",
        "BIPOLAR",
        "STEP",
        "COMPLEMENT",
        "ABSOLUTE",
        "SQUARE",
        "Cube",
        "SQRT",
        "StdInverse",
        "Exponential",
        "LogSigmoid",
        "ISRU",
    ];
    for name in SQUASHES {
        run_parity_squash(name, 4, 8, 512);
    }
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

// --- Issue #312: aggregate (MINIMUM/MAXIMUM/IF) + constant-neuron parity ------

/// Build a creature whose hidden layer uses an aggregate squash
/// (MINIMUM/MAXIMUM/IF). For IF, the input→hidden synapses cycle through the
/// three synapse types so the condition/positive/negative buckets are all
/// exercised; for MIN/MAX the type is irrelevant and left standard. The output
/// layer is IDENTITY summing the hidden activations (Issue #312).
fn aggregate_creature_json(
    num_inputs: usize,
    num_outputs: usize,
    hidden: usize,
    squash: &str,
) -> String {
    let mut neurons: Vec<String> = Vec::new();
    for h in 0..hidden {
        // Non-zero bias so the aggregate's `+ bias` term is exercised.
        neurons.push(format!(
            r#"{{"type":"hidden","uuid":"hidden-{h}","bias":0.1,"squash":"{squash}"}}"#
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
            let w = 0.3 + 0.05 * ((i * hidden + h) as f64);
            // Cycle condition/negative/positive/standard for IF; MIN/MAX ignore.
            let ty = match (i * hidden + h) % 4 {
                0 => r#","type":"condition""#,
                1 => r#","type":"negative""#,
                2 => r#","type":"positive""#,
                _ => "",
            };
            synapses.push(format!(
                r#"{{"fromUUID":"input-{i}","toUUID":"hidden-{h}","weight":{w}{ty}}}"#
            ));
        }
    }
    for h in 0..hidden {
        for o in 0..num_outputs {
            let w = 0.2 + 0.01 * ((h * num_outputs + o) as f64);
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

/// CPU↔GPU parity for a creature JSON string, replicated `num_creatures` times.
/// Shared by the aggregate and constant-neuron tests (Issue #312).
fn run_parity_json(label: &str, json: &str, num_creatures: usize, n_records: usize) {
    if resolve_backend(GpuMode::Auto)
        .map(|b| b.as_str() == "cpu-fallback")
        .unwrap_or(true)
    {
        eprintln!("skipping GPU parity for {label}: no compatible adapter");
        return;
    }
    let ctx = match select_adapter() {
        Ok(Some(c)) => Arc::new(c),
        _ => {
            eprintln!("skipping GPU parity for {label}: no context");
            return;
        }
    };

    let creature = parse_creature_json(json).expect("parse creature");
    let template = compile_creature(&creature).expect("compile");
    let num_inputs = template.num_inputs;
    let num_outputs = creature.output;
    let mut nets: Vec<_> = (0..num_creatures).map(|_| template.clone()).collect();

    let records = build_varied_records(num_inputs, num_outputs, n_records);

    let cpu_sums: Vec<f64> = nets
        .iter_mut()
        .map(|net| mse_sum_batch_packed(net, &records, num_inputs, num_outputs, true))
        .collect();

    let mut runner = BatchedRunner::new(ctx, &nets, num_inputs, num_outputs)
        .unwrap_or_else(|e| panic!("{label} must be GPU-supported: {e:?}"));
    let gpu_sums = runner
        .score_chunk(&records, n_records)
        .expect("GPU readback should succeed");

    assert_eq!(cpu_sums.len(), gpu_sums.len());
    for (i, (cpu, gpu)) in cpu_sums.iter().zip(gpu_sums.iter()).enumerate() {
        let denom = cpu.abs().max(1e-9);
        let rel = (cpu - gpu).abs() / denom;
        assert!(
            rel < 1e-3,
            "{label} creature {i}: CPU={cpu} GPU={gpu} relative_error={rel} exceeds 1e-3",
        );
    }
}

#[test]
fn cpu_vs_gpu_minimum_aggregate() {
    let json = aggregate_creature_json(8, 2, 6, "MINIMUM");
    run_parity_json("MINIMUM", &json, 4, 512);
}

#[test]
fn cpu_vs_gpu_maximum_aggregate() {
    let json = aggregate_creature_json(8, 2, 6, "MAXIMUM");
    run_parity_json("MAXIMUM", &json, 4, 512);
}

#[test]
fn cpu_vs_gpu_if_aggregate() {
    // IF exercises the condition/positive/negative synapse-type buckets on the
    // GPU (needs `SynapseGpu.synapse_type` — Issue #312).
    let json = aggregate_creature_json(8, 2, 6, "IF");
    run_parity_json("IF", &json, 4, 512);
}

/// Issue #312: the real GRQ-cluster creature (aggregates + constant neurons)
/// must now be GPU-hostable and score within tolerance of the CPU path. Gated
/// on `BENCH_PROD_CREATURE` pointing at a downloaded `network.json` (the 3 MB
/// evolved creature is not committed), and skipped when no GPU is present, so
/// CI stays green while a Metal/Vulkan host validates the production blocker.
#[test]
fn cpu_vs_gpu_real_prod_creature_when_available() {
    let path = match std::env::var("BENCH_PROD_CREATURE") {
        Ok(p) if !p.is_empty() => p,
        _ => {
            eprintln!("skipping real-creature parity: BENCH_PROD_CREATURE unset");
            return;
        }
    };
    let json = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("skipping real-creature parity: cannot read {path}: {e}");
            return;
        }
    };
    // Smaller corpus / pool than the bench — this is a correctness gate, not a
    // timing run — but still exercises the full aggregate + constant mix.
    run_parity_json("GRQ-prod", &json, 2, 128);
}

#[test]
fn cpu_vs_gpu_mixed_aggregates_and_constant_neuron() {
    // One creature carrying every Issue #312 blocker at once: a MINIMUM, a
    // MAXIMUM, an IF hidden neuron, plus a constant neuron — mirroring the real
    // GRQ-cluster creature's mix. Output sums the four hidden activations.
    let json = r#"{
        "input":4,"output":1,"forwardOnly":true,"semanticVersion":"4.0.0",
        "neurons":[
            {"type":"hidden","uuid":"h-min","bias":0.1,"squash":"MINIMUM"},
            {"type":"hidden","uuid":"h-max","bias":-0.2,"squash":"MAXIMUM"},
            {"type":"hidden","uuid":"h-if","bias":0.05,"squash":"IF"},
            {"type":"constant","uuid":"h-const","bias":0.73,"squash":"IDENTITY"},
            {"type":"output","uuid":"out-0","bias":0.0,"squash":"IDENTITY"}
        ],
        "synapses":[
            {"fromUUID":"input-0","toUUID":"h-min","weight":0.4},
            {"fromUUID":"input-1","toUUID":"h-min","weight":0.7},
            {"fromUUID":"input-2","toUUID":"h-min","weight":-0.5},
            {"fromUUID":"input-0","toUUID":"h-max","weight":0.6},
            {"fromUUID":"input-2","toUUID":"h-max","weight":-0.3},
            {"fromUUID":"input-3","toUUID":"h-max","weight":0.9},
            {"fromUUID":"input-0","toUUID":"h-if","weight":0.5,"type":"condition"},
            {"fromUUID":"input-1","toUUID":"h-if","weight":0.8,"type":"positive"},
            {"fromUUID":"input-2","toUUID":"h-if","weight":0.6,"type":"negative"},
            {"fromUUID":"input-3","toUUID":"h-const","weight":0.5},
            {"fromUUID":"h-min","toUUID":"out-0","weight":0.3},
            {"fromUUID":"h-max","toUUID":"out-0","weight":0.25},
            {"fromUUID":"h-if","toUUID":"out-0","weight":0.2},
            {"fromUUID":"h-const","toUUID":"out-0","weight":0.15}
        ]
    }"#;
    run_parity_json("mixed+constant", json, 4, 400);
}
