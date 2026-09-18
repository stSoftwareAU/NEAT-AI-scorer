//! Issue #574 — CPU/GPU parity contract for `IF`-heavy decision-tree creatures.
//!
//! `NEAT-AI-Forests` will generate tree-shaped `IF` creatures at scale and trust
//! this scorer as the final judge, so the branch semantics must be locked before
//! that traffic arrives. This suite pins three things:
//!
//! 1. **CPU branch semantics are exact.** Every fixture in
//!    [`rust_scorer::if_tree_fixture`] is scored through the real compiled
//!    network and compared against an independent reference evaluator — bit-for
//!    -bit, including the `condition == 0` boundary (which must take the
//!    negative branch). These assertions run everywhere, GPU or not.
//! 2. **`IF` is never reinterpreted as a point-wise squash.** A tree's
//!    prediction is always exactly one of its leaf constants, and it differs
//!    from what a sum-then-squash reading of the same neuron would produce.
//! 3. **GPU agrees with CPU, on both kernels.** When an adapter is present the
//!    same fixtures run through `BatchedRunner` (private kernel) and the
//!    scratch kernel, and a whole candidate batch is scored through the
//!    directory path so **candidate ordering** — the thing Forests actually
//!    consumes — is compared, not just individual losses.
//!
//! **Documented tolerance.** CPU↔CPU-reference comparisons are exact (`==` on
//! `f32`). Cross-backend comparisons use the repository's established `1e-3`
//! relative tolerance on the per-creature loss (Issue #82/#312), and candidate
//! **ordering** must match exactly.
//!
//! GPU bodies skip cleanly when no adapter is available so CPU-only CI passes;
//! the CPU assertions above still gate every run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use neat_core::creature::{compile_creature, parse_creature_json};
use neat_core::loss::mse_sum_batch_packed;
use neat_core::network::CompiledNetwork;

use rust_scorer::cost::CostKind;
use rust_scorer::gpu::forward_mse_batched::{BatchedRunner, KernelKind, MAX_NEURONS_PER_CREATURE};
use rust_scorer::gpu::{GpuBackendLabel, GpuContext, GpuMode, resolve_backend, select_adapter};
use rust_scorer::if_tree_fixture::{
    TreeSpec, boundary_records, corpus_records, grafted_creature_json,
    mixed_neural_if_creature_json, records_to_le_bytes, tree_creature_json, tree_reference_output,
    ungrafted_creature_json, varied_inputs,
};
use rust_scorer::multi_score::{
    gpu_directory_compatible, score_from_creature_dir, score_from_creature_dir_gpu,
};
use rust_scorer::scoring::ScoreResult;

/// Cross-backend relative tolerance on a per-creature loss (Issue #82/#312).
const CROSS_BACKEND_REL_TOLERANCE: f64 = 1e-3;

const NUM_INPUTS: usize = 6;

fn compile(json: &str) -> CompiledNetwork {
    let creature = parse_creature_json(json).expect("fixture parses");
    compile_creature(&creature).expect("fixture compiles")
}

/// Split a packed `inputs || target` corpus into one record's slices.
fn record_at(records: &[f32], num_inputs: usize, r: usize) -> (&[f32], f32) {
    let values_per_record = num_inputs + 1;
    let base = r * values_per_record;
    (
        &records[base..base + num_inputs],
        records[base + num_inputs],
    )
}

// --- 1. CPU branch semantics -------------------------------------------------

/// The compiled tree must reproduce the reference decision **exactly** at every
/// depth — the stump, the nested depth-2 tree and a depth-3 tree.
#[test]
fn cpu_tree_activation_matches_reference_at_every_depth() {
    for depth in 1..=3u32 {
        let spec = TreeSpec::new(NUM_INPUTS, depth, 1_000 + u64::from(depth));
        let mut net = compile(&tree_creature_json(&spec));
        for r in 0..512 {
            let inputs = varied_inputs(NUM_INPUTS, r);
            let actual = net.activate(&inputs, 1)[0];
            let expected = tree_reference_output(&spec, &inputs);
            assert_eq!(
                actual, expected,
                "depth {depth} record {r}: scorer picked a different branch than the reference"
            );
        }
    }
}

/// Every prediction is exactly one leaf constant. A point-wise reading of `IF`
/// (sum every input, then squash) produces a weighted sum instead, so this
/// invariant fails loudly if the aggregate is ever collapsed into `activate()`.
#[test]
fn cpu_tree_prediction_is_always_one_of_the_leaf_constants() {
    let spec = TreeSpec::new(NUM_INPUTS, 3, 4_242);
    let leaves = spec.leaf_values();
    let mut net = compile(&tree_creature_json(&spec));
    for r in 0..512 {
        let inputs = varied_inputs(NUM_INPUTS, r);
        let out = net.activate(&inputs, 1)[0];
        assert!(
            leaves.contains(&out),
            "record {r}: prediction {out} is not a leaf constant ({leaves:?}) — IF may have \
             been reinterpreted as a point-wise squash"
        );
    }
}

/// The fixture must be *discriminating*: a point-wise reinterpretation of the
/// root `IF` (sum of every weighted input, identity squash) has to disagree with
/// the branch semantics on a large share of records. Without this, test 2 could
/// pass on a broken scorer by coincidence.
#[test]
fn pointwise_reinterpretation_would_change_the_answer() {
    let spec = TreeSpec::new(NUM_INPUTS, 1, 77);
    let mut net = compile(&tree_creature_json(&spec));
    let feature = spec.feature(0);
    let threshold = spec.threshold(0) as f32;

    let mut disagreements = 0;
    for r in 0..256 {
        let inputs = varied_inputs(NUM_INPUTS, r);
        let branch = net.activate(&inputs, 1)[0];
        // What a point-wise `IF` would emit: every weighted input summed, no
        // bucketing by synapse role.
        let pointwise =
            inputs[feature] - threshold + spec.leaf_value(1) as f32 + spec.leaf_value(0) as f32;
        if (branch - pointwise).abs() > 1e-6 {
            disagreements += 1;
        }
    }
    assert!(
        disagreements > 200,
        "only {disagreements}/256 records distinguish branch semantics from a point-wise sum"
    );
}

/// `condition == 0` takes the **negative** branch, and the two neighbouring ULPs
/// fall either side of it. This is the boundary Forests will sit on whenever a
/// threshold is learnt from the data itself.
#[test]
fn cpu_branch_boundary_at_condition_zero_takes_the_negative_branch() {
    let spec = TreeSpec::new(NUM_INPUTS, 1, 31);
    let mut net = compile(&tree_creature_json(&spec));
    let feature = spec.feature(0);
    let threshold = spec.threshold(0) as f32;
    let mut inputs = varied_inputs(NUM_INPUTS, 0);

    inputs[feature] = threshold;
    assert_eq!(
        net.activate(&inputs, 1)[0],
        spec.leaf_value(0) as f32,
        "condition == 0 must take the negative branch"
    );
    inputs[feature] = threshold.next_down();
    assert_eq!(
        net.activate(&inputs, 1)[0],
        spec.leaf_value(0) as f32,
        "condition < 0 must take the negative branch"
    );
    inputs[feature] = threshold.next_up();
    assert_eq!(
        net.activate(&inputs, 1)[0],
        spec.leaf_value(1) as f32,
        "condition > 0 must take the positive branch"
    );
}

/// The whole boundary corpus — every internal node pinned on, just below and
/// just above its threshold — must score exactly as the reference decides.
#[test]
fn cpu_boundary_corpus_matches_reference_for_a_nested_tree() {
    let spec = TreeSpec::new(NUM_INPUTS, 3, 555);
    let records = boundary_records(&spec);
    let n_records = records.len() / (NUM_INPUTS + 1);
    let mut net = compile(&tree_creature_json(&spec));
    for r in 0..n_records {
        let (inputs, target) = record_at(&records, NUM_INPUTS, r);
        let actual = net.activate(inputs, 1)[0];
        assert_eq!(
            actual, target,
            "boundary record {r}: scorer disagreed with the reference branch"
        );
    }
}

/// Ordinary point-wise neurons feeding an `IF` condition still produce an exact
/// branch constant — the mixed-topology case.
#[test]
fn cpu_mixed_neural_and_if_creature_still_branches_exactly() {
    let spec = TreeSpec::new(NUM_INPUTS, 1, 909);
    let mut net = compile(&mixed_neural_if_creature_json(&spec, 5));
    let leaves = [spec.leaf_value(0) as f32, spec.leaf_value(1) as f32];
    let mut seen_positive = false;
    let mut seen_negative = false;
    for r in 0..256 {
        let inputs = varied_inputs(NUM_INPUTS, r);
        let out = net.activate(&inputs, 1)[0];
        assert!(
            leaves.contains(&out),
            "record {r}: mixed creature emitted {out}, not a branch constant {leaves:?}"
        );
        seen_positive |= out == leaves[1];
        seen_negative |= out == leaves[0];
    }
    assert!(
        seen_positive && seen_negative,
        "mixed fixture must exercise both branches (positive={seen_positive}, negative={seen_negative})"
    );
}

/// The appended `IF` correction graft must actually move the prediction —
/// otherwise a grafted-candidate parity test would be vacuous.
#[test]
fn cpu_if_graft_changes_the_large_creature_prediction() {
    let spec = TreeSpec::new(NUM_INPUTS, 1, 606);
    let hidden = 32;
    let mut grafted = compile(&grafted_creature_json(&spec, hidden));
    let mut plain = compile(&ungrafted_creature_json(&spec, hidden));
    let mut changed = 0;
    for r in 0..128 {
        let inputs = varied_inputs(NUM_INPUTS, r);
        let with = grafted.activate(&inputs, 1)[0];
        let without = plain.activate(&inputs, 1)[0];
        if (with - without).abs() > 1e-6 {
            changed += 1;
        }
    }
    assert_eq!(
        changed,
        128,
        "the IF graft left {} records untouched",
        128 - changed
    );
}

// --- 2. Fail closed to CPU ---------------------------------------------------

/// A directory of `IF` trees is GPU-hostable (the kernels reduce `IF` inline
/// since Issue #312) — the routing decision must not quietly refuse them.
#[test]
fn if_tree_directory_is_reported_gpu_compatible() {
    let root = temp_root("if_tree_compat");
    let (creatures_dir, _data_dir) = write_candidate_fixture(&root, 4, 2, 32);
    gpu_directory_compatible(&creatures_dir)
        .expect("IF trees must be GPU-hostable — they are reduced inline by both kernels");
}

/// An unsupported aggregate (`HYPOT`, discriminant 35) still fails pre-flight so
/// the run falls **closed** to the CPU pipeline rather than silently scoring the
/// neuron as a point-wise squash.
#[test]
fn unsupported_aggregate_fails_closed_to_cpu() {
    let root = temp_root("if_tree_unsupported");
    let (creatures_dir, _data_dir) = write_candidate_fixture(&root, 2, 1, 16);
    // A creature whose root aggregate is not hosted by either kernel.
    let hypot = tree_creature_json(&TreeSpec::new(NUM_INPUTS, 1, 3)).replace("\"IF\"", "\"HYPOT\"");
    std::fs::write(creatures_dir.join("hypot.json"), hypot).expect("write hypot creature");

    let err = gpu_directory_compatible(&creatures_dir)
        .expect_err("an unhosted aggregate must refuse the GPU path");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("UnsupportedSquash"),
        "expected an UnsupportedSquash refusal, got {msg}"
    );
}

/// With no GPU in play (`GpuBackendLabel::CpuFallback`) the directory path still
/// scores an `IF` candidate batch, and the per-candidate losses match the
/// reference decision computed record by record. This is the fail-closed path
/// Forests gets on a GPU-less host.
#[test]
fn cpu_fallback_directory_scoring_matches_the_reference_losses() {
    let root = temp_root("if_tree_cpu_dir");
    let n_candidates = 5;
    let n_records = 512;
    let (creatures_dir, data_dir) = write_candidate_fixture(&root, n_candidates, 2, n_records);

    let scores = score_from_creature_dir(
        &creatures_dir,
        &data_dir,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
    )
    .expect("CPU directory scoring");
    assert_eq!(scores.len(), n_candidates);

    let records = corpus_records(&oracle_spec(), n_records);
    for (name, result) in &scores {
        let spec = candidate_spec(name);
        let mut expected = 0.0f64;
        for r in 0..n_records {
            let (inputs, target) = record_at(&records, NUM_INPUTS, r);
            let d = f64::from(target - tree_reference_output(&spec, inputs));
            expected += d * d;
        }
        expected /= n_records as f64;
        assert!(
            (result.error - expected).abs() <= 1e-9 * expected.max(1.0),
            "{name}: scorer error {} != reference error {expected}",
            result.error
        );
        assert_eq!(result.record_count, n_records);
    }
}

// --- 3. GPU parity -----------------------------------------------------------

/// Acquire a GPU context, or `None` when the host has no adapter.
fn gpu_context(label: &str) -> Option<Arc<GpuContext>> {
    if resolve_backend(GpuMode::Auto)
        .map(|b| b.as_str() == "cpu-fallback")
        .unwrap_or(true)
    {
        eprintln!("skipping {label}: no compatible adapter");
        return None;
    }
    match select_adapter() {
        Ok(Some(c)) if c.backend != GpuBackendLabel::CpuFallback => Some(Arc::new(c)),
        _ => {
            eprintln!("skipping {label}: select_adapter returned no context");
            None
        }
    }
}

/// Score `json` (replicated `num_creatures` times) on CPU and GPU and assert the
/// per-creature losses agree within the documented tolerance, on the expected
/// kernel.
fn assert_kernel_parity(
    label: &str,
    json: &str,
    num_creatures: usize,
    records: &[f32],
    expected_kernel: KernelKind,
) {
    let Some(ctx) = gpu_context(label) else {
        return;
    };
    let template = compile(json);
    let num_inputs = template.num_inputs;
    let n_records = records.len() / (num_inputs + 1);
    let mut nets: Vec<CompiledNetwork> = (0..num_creatures).map(|_| template.clone()).collect();

    let cpu: Vec<f64> = nets
        .iter_mut()
        .map(|net| mse_sum_batch_packed(net, records, num_inputs, 1, true))
        .collect();

    let mut runner = BatchedRunner::new(ctx, &nets, num_inputs, 1, CostKind::Mse)
        .unwrap_or_else(|e| panic!("{label} must be GPU-hostable: {e:?}"));
    assert_eq!(
        runner.kernel(),
        expected_kernel,
        "{label} routed to the wrong kernel"
    );
    let gpu = runner
        .score_chunk(records, n_records)
        .expect("GPU readback");

    assert_eq!(cpu.len(), gpu.len());
    for (i, (c, g)) in cpu.iter().zip(gpu.iter()).enumerate() {
        let rel = (c - g).abs() / c.abs().max(1e-9);
        assert!(
            rel < CROSS_BACKEND_REL_TOLERANCE,
            "{label} creature {i}: CPU={c} GPU={g} relative_error={rel} exceeds \
             {CROSS_BACKEND_REL_TOLERANCE}"
        );
    }
}

#[test]
fn gpu_matches_cpu_for_a_depth_1_stump() {
    let spec = TreeSpec::new(NUM_INPUTS, 1, 17);
    let records = corpus_records(&oracle_spec(), 2_048);
    assert_kernel_parity(
        "stump",
        &tree_creature_json(&spec),
        4,
        &records,
        KernelKind::Private,
    );
}

#[test]
fn gpu_matches_cpu_for_a_nested_tree() {
    let spec = TreeSpec::new(NUM_INPUTS, 3, 18);
    let records = corpus_records(&oracle_spec(), 2_048);
    assert_kernel_parity(
        "nested-tree",
        &tree_creature_json(&spec),
        4,
        &records,
        KernelKind::Private,
    );
}

#[test]
fn gpu_matches_cpu_for_mixed_neural_and_if_neurons() {
    let spec = TreeSpec::new(NUM_INPUTS, 1, 19);
    let records = corpus_records(&oracle_spec(), 2_048);
    assert_kernel_parity(
        "mixed-neural-if",
        &mixed_neural_if_creature_json(&spec, 8),
        4,
        &records,
        KernelKind::Private,
    );
}

/// The Forests shape that matters for routing: a large creature above the
/// 256-neuron private cap carrying a small appended `IF` correction graft, so
/// the **scratch** kernel's `IF` reduction is covered too.
#[test]
fn gpu_matches_cpu_for_a_large_creature_with_an_if_graft() {
    let spec = TreeSpec::new(NUM_INPUTS, 1, 20);
    // inputs + hidden + constant + IF + output must exceed the private cap.
    let hidden = MAX_NEURONS_PER_CREATURE as usize + 32;
    let records = corpus_records(&oracle_spec(), 512);
    assert_kernel_parity(
        "grafted-large",
        &grafted_creature_json(&spec, hidden),
        2,
        &records,
        KernelKind::Scratch,
    );
}

/// Branch-boundary records must not disagree across backends: the whole
/// boundary corpus is scored on both and compared.
#[test]
fn gpu_matches_cpu_on_branch_boundary_records() {
    let spec = TreeSpec::new(NUM_INPUTS, 3, 21);
    let records = boundary_records(&spec);
    assert_kernel_parity(
        "boundary",
        &tree_creature_json(&spec),
        2,
        &records,
        KernelKind::Private,
    );
}

/// The acceptance criterion Forests depends on: for a batch of candidate trees
/// scored against one corpus, GPU and CPU must rank the candidates **identically**
/// and agree on every loss within tolerance.
#[test]
fn gpu_candidate_ordering_matches_cpu() {
    let Some(ctx) = gpu_context("candidate-ordering") else {
        return;
    };
    let root = temp_root("if_tree_ordering");
    let n_candidates = 12;
    let n_records = 4_096;
    let (creatures_dir, data_dir) = write_candidate_fixture(&root, n_candidates, 3, n_records);

    let cpu = score_from_creature_dir(
        &creatures_dir,
        &data_dir,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
    )
    .expect("CPU directory scoring");
    let backend = ctx.backend;
    let gpu =
        score_from_creature_dir_gpu(&creatures_dir, &data_dir, backend, ctx, 1, CostKind::Mse)
            .expect("GPU directory scoring");

    assert_eq!(cpu.len(), gpu.len(), "both backends score every candidate");
    for (name, cpu_result) in &cpu {
        let gpu_result = gpu.get(name).expect("GPU scored the same candidate");
        let rel = (cpu_result.error - gpu_result.error).abs() / cpu_result.error.abs().max(1e-9);
        assert!(
            rel < CROSS_BACKEND_REL_TOLERANCE,
            "{name}: CPU error {} vs GPU error {} (relative {rel})",
            cpu_result.error,
            gpu_result.error
        );
    }
    assert_eq!(
        ranking(&cpu),
        ranking(&gpu),
        "candidate ordering must be identical across backends"
    );
}

/// Candidate names ordered best (lowest error) first.
fn ranking(scores: &BTreeMap<String, ScoreResult>) -> Vec<String> {
    let mut ranked: Vec<(&String, f64)> = scores.iter().map(|(k, v)| (k, v.error)).collect();
    // Ties break on name so the ordering is total and backend-independent.
    ranked.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(b.0)));
    ranked.into_iter().map(|(k, _)| k.clone()).collect()
}

// --- fixture plumbing --------------------------------------------------------

/// The tree whose predictions become the corpus targets.
fn oracle_spec() -> TreeSpec {
    TreeSpec::new(NUM_INPUTS, 2, 0)
}

/// Seed encoded in a candidate file name (`candidate-<depth>-<seed>`).
fn candidate_spec(name: &str) -> TreeSpec {
    let mut parts = name.split('-').skip(1);
    let depth: u32 = parts.next().and_then(|p| p.parse().ok()).expect("depth");
    let seed: u64 = parts.next().and_then(|p| p.parse().ok()).expect("seed");
    TreeSpec::new(NUM_INPUTS, depth, seed)
}

fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&root);
    root
}

/// Write `n_candidates` distinct candidate trees plus one `.bin` corpus whose
/// targets are the oracle tree's predictions.
fn write_candidate_fixture(
    root: &Path,
    n_candidates: usize,
    depth: u32,
    n_records: usize,
) -> (PathBuf, PathBuf) {
    let creatures_dir = root.join("creatures");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    for c in 0..n_candidates {
        let seed = 100 + c as u64;
        let spec = TreeSpec::new(NUM_INPUTS, depth, seed);
        std::fs::write(
            creatures_dir.join(format!("candidate-{depth}-{seed}.json")),
            tree_creature_json(&spec),
        )
        .expect("write candidate");
    }

    let records = corpus_records(&oracle_spec(), n_records);
    std::fs::write(data_dir.join("0.bin"), records_to_le_bytes(&records)).expect("write corpus");

    (creatures_dir, data_dir)
}
