//! Issue #581 — parity guard for `(from, to, type)`-keyed synapses.
//!
//! `NEAT-AI-core#577` relaxed the duplicate-synapse rule so one source may feed
//! an `IF` neuron through more than one role: the contribution that must apply
//! **whichever way the node branches** no longer needs an `IDENTITY` relay
//! purely to be a second distinct source. This engine is the one that would
//! *disagree* first if that relaxation were mishandled — it resolves every
//! synapse independently and sums each role's bucket, whereas a loader keyed by
//! `(from, to)` alone keeps one edge per ordered pair and silently drops the
//! rest. Two engines then score the same JSON differently, which is exactly the
//! divergence that produced a production "improvement" that was not real
//! (NEAT-AI-core#556: `rust_scorer` 0.356183 against `Creature.scoreDir`
//! 0.353147).
//!
//! What this suite pins, using the fixtures in
//! [`rust_scorer::dual_role_fixture`]:
//!
//! 1. **Nothing is dropped on load.** The relay-free creature parses and
//!    compiles with every synapse its JSON declares — the Rust-side spelling of
//!    the `jsonSynapses === loadedSynapses` assertion `NEAT-AI-Forests`'
//!    `ts_parity.rs` makes against `Creature.scoreDir`, and the assertion that
//!    would have caught the original divergence.
//! 2. **The relaxed form and the relay workaround are the same function.** They
//!    activate bit-identically and score identically through the real directory
//!    pipeline, so upstream may drop the relay without moving a single score.
//! 3. **A dropped edge is detectable, not assumed.** The creature a
//!    `(from, to)`-keyed loader is left holding scores *differently*, so test 1
//!    cannot pass vacuously.
//! 4. **Both GPU kernels agree.** The kernels bucket by synapse role too, so the
//!    relaxed shape is scored on GPU and compared against CPU when an adapter is
//!    present.
//!
//! **Documented tolerance.** CPU↔CPU comparisons — reference, relay-equivalence
//! and pipeline score — are exact (`==`), because both forms perform the same
//! `f32` arithmetic in the same order. Cross-backend comparisons use the
//! repository's established `1e-3` relative tolerance (Issue #82/#312/#574).
//!
//! GPU bodies skip cleanly when no adapter is available; the CPU assertions gate
//! every run.
//!
//! **Still open upstream.** NEAT-AI#3873 (the TypeScript half) has not landed —
//! `Creature.ts` still asserts `"Connection already exists"` on the pair alone.
//! These fixtures are `pub` so the cross-engine harness can score the *same*
//! creatures the moment it does; nothing here waits on that.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use neat_core::creature::{compile_creature, parse_creature_json};
use neat_core::loss::mse_sum_batch_packed;
use neat_core::network::CompiledNetwork;

use rust_scorer::cost::CostKind;
use rust_scorer::dual_role_fixture::{
    dropped_shared_branch_creature_json, dual_role_boundary_records, dual_role_corpus_records,
    dual_role_if_creature_json, dual_role_reference_output, relay_equivalent_if_creature_json,
    shared_feature,
};
use rust_scorer::gpu::forward_mse_batched::{BatchedRunner, KernelKind};
use rust_scorer::gpu::{GpuBackendLabel, GpuContext, GpuMode, resolve_backend, select_adapter};
use rust_scorer::if_tree_fixture::{TreeSpec, records_to_le_bytes, varied_inputs};
use rust_scorer::multi_score::score_from_creature_dir;
use rust_scorer::scoring::ScoreResult;

/// Cross-backend relative tolerance on a per-creature loss (Issue #82/#312).
const CROSS_BACKEND_REL_TOLERANCE: f64 = 1e-3;

const NUM_INPUTS: usize = 6;

/// The creature under test: one `IF` node whose condition, positive and
/// negative buckets are fed by repeated sources.
fn spec() -> TreeSpec {
    TreeSpec::new(NUM_INPUTS, 1, 581)
}

fn compile(json: &str) -> CompiledNetwork {
    let creature = parse_creature_json(json).expect("fixture parses");
    compile_creature(&creature).expect("fixture compiles")
}

/// Count `"fromUUID"` occurrences — the synapse count the *JSON* declares,
/// read without going through the loader under test.
fn declared_synapse_count(json: &str) -> usize {
    json.matches(r#""fromUUID""#).count()
}

// --- 1. Nothing is dropped on load -------------------------------------------

/// The Rust-side `jsonSynapses === loadedSynapses` assertion.
///
/// Counted three ways — from the raw JSON text, from the parsed export and from
/// the compiled network — so neither the parser nor the compiler can quietly
/// collapse a repeated `(from, to)` pair.
#[test]
fn every_declared_synapse_survives_the_load() {
    let spec = spec();
    for (label, json) in [
        ("relay-free", dual_role_if_creature_json(&spec)),
        ("relay-equivalent", relay_equivalent_if_creature_json(&spec)),
    ] {
        let declared = declared_synapse_count(&json);
        let creature = parse_creature_json(&json).expect("parses");
        let net = compile_creature(&creature).expect("compiles");
        assert_eq!(
            creature.synapses.len(),
            declared,
            "{label}: parsing dropped {} of {declared} synapses",
            declared - creature.synapses.len()
        );
        assert_eq!(
            net.synapses.len(),
            declared,
            "{label}: compiling dropped {} of {declared} synapses",
            declared - net.synapses.len()
        );
    }
}

/// The relay-free creature really does repeat an ordered pair — otherwise the
/// assertion above would be testing nothing. One source (`input-<shared>`)
/// reaches the `IF` node twice, once per branch role.
#[test]
fn the_fixture_actually_repeats_an_ordered_pair() {
    let spec = spec();
    let creature = parse_creature_json(&dual_role_if_creature_json(&spec)).expect("parses");
    let shared = format!("input-{}", shared_feature(&spec));
    let repeats = creature
        .synapses
        .iter()
        .filter(|s| s.from_uuid == shared)
        .count();
    assert_eq!(
        repeats, 2,
        "the shared source must feed the IF node through both branch roles"
    );

    let mut pairs = std::collections::HashSet::new();
    let duplicated = creature
        .synapses
        .iter()
        .filter(|s| !pairs.insert((s.from_uuid.clone(), s.to_uuid.clone())))
        .count();
    assert!(
        duplicated > 0,
        "a (from, to)-keyed loader would drop nothing from this fixture"
    );
}

// --- 2. The relaxed form and the relay workaround agree ----------------------

/// The compiled relay-free creature reproduces the independent reference
/// evaluation bit-for-bit, including the shared term applied on both branches.
#[test]
fn cpu_activation_matches_the_independent_reference() {
    let spec = spec();
    let mut net = compile(&dual_role_if_creature_json(&spec));
    for r in 0..512 {
        let inputs = varied_inputs(NUM_INPUTS, r);
        assert_eq!(
            net.activate(&inputs, 1)[0],
            dual_role_reference_output(&spec, &inputs),
            "record {r}: the dual-role IF node did not evaluate as the reference decides"
        );
    }
}

/// Relaxing the rule must not move a number: the relay-free creature and the
/// pre-#577 relay workaround activate identically on every record.
#[test]
fn the_relay_free_and_relay_forms_activate_identically() {
    let spec = spec();
    let mut relaxed = compile(&dual_role_if_creature_json(&spec));
    let mut relayed = compile(&relay_equivalent_if_creature_json(&spec));
    for r in 0..512 {
        let inputs = varied_inputs(NUM_INPUTS, r);
        assert_eq!(
            relaxed.activate(&inputs, 1)[0],
            relayed.activate(&inputs, 1)[0],
            "record {r}: dropping the IDENTITY relay changed the prediction"
        );
    }
}

/// `condition == 0` takes the **negative** branch (Issue #574's contract), and
/// the shared source's negative edge still applies there — that edge is the one
/// a `(from, to)`-keyed loader drops.
#[test]
fn the_condition_zero_boundary_keeps_the_shared_negative_edge() {
    let spec = spec();
    let records = dual_role_boundary_records(&spec);
    let width = NUM_INPUTS + 1;
    let mut net = compile(&dual_role_if_creature_json(&spec));
    let mut dropped = compile(&dropped_shared_branch_creature_json(&spec));

    let mut negative_records = 0;
    for r in 0..records.len() / width {
        let base = r * width;
        let inputs = &records[base..base + NUM_INPUTS];
        let expected = records[base + NUM_INPUTS];
        assert_eq!(
            net.activate(inputs, 1)[0],
            expected,
            "boundary record {r}: scorer disagreed with the reference branch"
        );
        // On the negative branch the intact creature must differ from the one
        // missing that branch's shared edge.
        if net.activate(inputs, 1)[0] != dropped.activate(inputs, 1)[0] {
            negative_records += 1;
        }
    }
    assert!(
        negative_records > 0,
        "no boundary record took the negative branch — the guard proves nothing"
    );
}

// --- 3. A dropped edge is detectable -----------------------------------------

/// The creature a `(from, to)`-keyed loader is left holding predicts something
/// else. Without this, "nothing was dropped" could hold trivially.
#[test]
fn dropping_the_shared_negative_edge_changes_the_prediction() {
    let spec = spec();
    let mut intact = compile(&dual_role_if_creature_json(&spec));
    let mut dropped = compile(&dropped_shared_branch_creature_json(&spec));
    let changed = (0..512)
        .filter(|r| {
            let inputs = varied_inputs(NUM_INPUTS, *r);
            intact.activate(&inputs, 1)[0] != dropped.activate(&inputs, 1)[0]
        })
        .count();
    assert!(
        changed > 100,
        "only {changed}/512 records notice the dropped branch edge — the divergence \
         would be invisible"
    );
}

// --- Full-pipeline scoring ---------------------------------------------------

/// Both forms scored through the real directory pipeline must return the same
/// loss, and the dropped variant must return a different one.
///
/// This is the end-to-end shape of the cross-engine comparison: the same corpus,
/// the same cost, three creatures, scored the way production scores them.
#[test]
fn directory_scoring_agrees_between_the_forms_and_separates_the_dropped_one() {
    let spec = spec();
    let root = temp_root("dual_role_directory");
    let creatures_dir = root.join("creatures");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    std::fs::write(
        creatures_dir.join("relaxed.json"),
        dual_role_if_creature_json(&spec),
    )
    .expect("write relaxed creature");
    std::fs::write(
        creatures_dir.join("relayed.json"),
        relay_equivalent_if_creature_json(&spec),
    )
    .expect("write relayed creature");
    std::fs::write(
        creatures_dir.join("dropped.json"),
        dropped_shared_branch_creature_json(&spec),
    )
    .expect("write dropped creature");

    let n_records = 2_048;
    let records = dual_role_corpus_records(&spec, n_records);
    std::fs::write(data_dir.join("0.bin"), records_to_le_bytes(&records)).expect("write corpus");

    let scores: BTreeMap<String, ScoreResult> = score_from_creature_dir(
        &creatures_dir,
        &data_dir,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
    )
    .expect("CPU directory scoring");
    assert_eq!(scores.len(), 3, "every candidate must be scored");

    let error = |name: &str| scores.get(name).expect("candidate scored").error;
    assert_eq!(
        error("relaxed"),
        error("relayed"),
        "the relay-free creature and its relay workaround must score identically"
    );
    assert_ne!(
        error("relaxed"),
        error("dropped"),
        "losing a branch edge must move the loss — otherwise the parity guard is vacuous"
    );
    for name in ["relaxed", "relayed", "dropped"] {
        let result = scores.get(name).expect("candidate scored");
        assert_eq!(result.record_count, n_records);
        assert!(
            result.error > 0.0,
            "{name}: a zero loss would make the comparison vacuous"
        );
    }
}

// --- 4. GPU parity ------------------------------------------------------------

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

/// Score `json` on CPU and GPU and assert the per-creature losses agree within
/// the documented cross-backend tolerance.
fn assert_gpu_parity(label: &str, json: &str, records: &[f32]) {
    let Some(ctx) = gpu_context(label) else {
        return;
    };
    let template = compile(json);
    let num_inputs = template.num_inputs;
    let n_records = records.len() / (num_inputs + 1);
    let mut nets: Vec<CompiledNetwork> = (0..4).map(|_| template.clone()).collect();

    let cpu: Vec<f64> = nets
        .iter_mut()
        .map(|net| mse_sum_batch_packed(net, records, num_inputs, 1, true))
        .collect();

    let mut runner = BatchedRunner::new(ctx, &nets, num_inputs, 1, CostKind::Mse)
        .unwrap_or_else(|e| panic!("{label} must be GPU-hostable: {e:?}"));
    assert_eq!(
        runner.kernel(),
        KernelKind::Private,
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

/// The kernels bucket by synapse role as well, so a source feeding two roles is
/// a shape they have to get right.
#[test]
fn gpu_matches_cpu_for_the_dual_role_creature() {
    let spec = spec();
    let records = dual_role_corpus_records(&spec, 2_048);
    assert_gpu_parity("dual-role", &dual_role_if_creature_json(&spec), &records);
}

/// The relay workaround is the creature production carries today — it must keep
/// scoring the same on GPU as the relaxed form replacing it.
#[test]
fn gpu_matches_cpu_for_the_relay_equivalent_creature() {
    let spec = spec();
    let records = dual_role_corpus_records(&spec, 2_048);
    assert_gpu_parity(
        "relay-equivalent",
        &relay_equivalent_if_creature_json(&spec),
        &records,
    );
}

/// Branch-boundary records — condition on, just below and just above the
/// threshold — must not disagree across backends either.
#[test]
fn gpu_matches_cpu_on_the_dual_role_boundary_records() {
    let spec = spec();
    let records = dual_role_boundary_records(&spec);
    assert_gpu_parity(
        "dual-role-boundary",
        &dual_role_if_creature_json(&spec),
        &records,
    );
}

fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&root);
    root
}
