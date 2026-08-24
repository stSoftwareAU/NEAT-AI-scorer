//! Cross-engine fixtures for `(from, to, type)`-keyed synapses — Issue #581.
//!
//! An `IF` neuron keeps a separate sum per synapse role, so a contribution that
//! must apply **whichever way the node branches** needs two synapses into it —
//! one `positive`, one `negative`. Under the old `(from, to)` uniqueness rule
//! those two could not share a source, and the workaround was an `IDENTITY`
//! relay neuron existing purely to be a second distinct source (455 of them had
//! accumulated on one production creature). `NEAT-AI-core#577` relaxed the key
//! to the `(from, to, type)` triple for `IF` targets, so the relay is no longer
//! needed.
//!
//! This module builds the pair of creatures that pins what the scorer does with
//! that shape:
//!
//! ```text
//!   relay-free (post-#577)              relay workaround (pre-#577)
//!
//!   input-c ──cond──▶                   input-c ──cond──▶
//!   const-1 ──cond──▶                   const-1 ──cond──▶
//!   const-1 ──pos───▶  IF               const-1-pos ──pos───▶  IF
//!   const-1 ──neg───▶ node              const-1-neg ──neg───▶ node
//!   input-s ──pos───▶                   relay-pos ──pos───▶
//!   input-s ──neg───▶                   relay-neg ──neg───▶
//!                                         ▲ ▲
//!                                    input-s ┘ └ input-s
//! ```
//!
//! Both forms describe the *same* function, so the scorer must give them the
//! *same* score — that equality is the parity assertion, and it is exact rather
//! than approximate because the relay contributes `0.0 + 1.0 * x`.
//!
//! ## Why it is a cross-engine fixture
//!
//! `rust_scorer` resolves every synapse independently, so it keeps all six
//! edges into the `IF` node and sums each role's bucket. A loader that keys by
//! `(from, to)` alone keeps only one edge per pair and silently drops the
//! second — measured against NEAT-AI 6.6.39, a creature of this shape loads
//! with `jsonSynapses = 4` but `loadedSynapses = 3`. The two engines then score
//! the same JSON differently, which is exactly the divergence that produced a
//! production "improvement" that was not real (Issue #556). The creatures here
//! are emitted through [`crate::fixture_json`] in the shape **both** engines
//! accept, so `NEAT-AI-Forests`' `ts_parity.rs` harness can score them under
//! `Creature.scoreDir` and compare against `jsonSynapses === loadedSynapses`
//! once `NEAT-AI#3873` lands the TypeScript half of the rule.
//!
//! [`dropped_shared_branch_creature_json`] models the creature a `(from, to)`
//! -keyed loader is left holding, so a test can prove the dropped synapse
//! actually changes the answer rather than assuming it.

use crate::fixture_json::{
    constant_neuron_json, creature_envelope, neuron_json, synapse_json, typed_synapse_json,
};
use crate::if_tree_fixture::{TreeSpec, varied_inputs};

/// UUID of the single constant every fixture here hangs its threshold and both
/// branch constants off — one neuron for all three roles, which is the whole
/// point of the relaxed rule.
pub const CONST_ONE_UUID: &str = "const-one";

/// UUID of the `IF` neuron under test.
pub const IF_UUID: &str = "if-0";

/// UUID of the single output neuron.
pub const OUTPUT_UUID: &str = "output-0";

/// UUID of the `IDENTITY` relay standing in for the shared source's `positive`
/// edge in the pre-#577 workaround form.
pub const RELAY_POSITIVE_UUID: &str = "relay-positive";

/// UUID of the `IDENTITY` relay standing in for the shared source's `negative`
/// edge in the pre-#577 workaround form.
pub const RELAY_NEGATIVE_UUID: &str = "relay-negative";

/// UUID of the constant feeding the `positive` branch in the workaround form —
/// the second of the three constants #577 makes unnecessary.
pub const CONST_ONE_POSITIVE_UUID: &str = "const-one-positive";

/// UUID of the constant feeding the `negative` branch in the workaround form.
pub const CONST_ONE_NEGATIVE_UUID: &str = "const-one-negative";

/// Weight of the shared contribution that must apply on **either** branch.
///
/// Non-zero and not a power of two so a dropped copy cannot coincidentally
/// cancel or round away.
pub const SHARED_BRANCH_WEIGHT: f64 = 0.35;

/// Input column the `IF` node splits on.
#[must_use]
pub fn condition_feature(spec: &TreeSpec) -> usize {
    spec.feature(0)
}

/// Input column whose contribution is shared across both branches.
///
/// Distinct from [`condition_feature`] whenever the creature reads more than
/// one column, so the shared term cannot be confused with the condition term.
#[must_use]
pub fn shared_feature(spec: &TreeSpec) -> usize {
    (spec.feature(0) + 1) % spec.num_inputs
}

/// The `IF` node's condition, positive and negative edges, relay-free.
///
/// Emission order is fixed — `condition(input)`, `condition(-threshold)`,
/// `positive(leaf high)`, `negative(leaf low)`, `positive(shared)`,
/// `negative(shared)` — because `compile_creature` preserves it and
/// [`dual_role_reference_output`] sums each bucket in the same order. That makes
/// the reference bit-exact against the CPU pipeline rather than merely close.
fn push_dual_role_synapses(spec: &TreeSpec, synapses: &mut Vec<String>) {
    synapses.push(typed_synapse_json(
        &format!("input-{}", condition_feature(spec)),
        IF_UUID,
        1.0,
        Some("condition"),
    ));
    synapses.push(typed_synapse_json(
        CONST_ONE_UUID,
        IF_UUID,
        -spec.threshold(0),
        Some("condition"),
    ));
    synapses.push(typed_synapse_json(
        CONST_ONE_UUID,
        IF_UUID,
        spec.leaf_value(1),
        Some("positive"),
    ));
    synapses.push(typed_synapse_json(
        CONST_ONE_UUID,
        IF_UUID,
        spec.leaf_value(0),
        Some("negative"),
    ));
    let shared = format!("input-{}", shared_feature(spec));
    synapses.push(typed_synapse_json(
        &shared,
        IF_UUID,
        SHARED_BRANCH_WEIGHT,
        Some("positive"),
    ));
    synapses.push(typed_synapse_json(
        &shared,
        IF_UUID,
        SHARED_BRANCH_WEIGHT,
        Some("negative"),
    ));
}

/// The relay-free creature: two sources each feed the `IF` node through more
/// than one role.
///
/// `const-one` carries all three roles (`condition`, `positive`, `negative`) and
/// the shared input column carries both branches. Seven synapses, three
/// neurons — the shape that is only legal once synapses are keyed by
/// `(from, to, type)`.
#[must_use]
pub fn dual_role_if_creature_json(spec: &TreeSpec) -> String {
    let neurons = vec![
        constant_neuron_json(CONST_ONE_UUID, 1.0),
        neuron_json("hidden", IF_UUID, 0.0, "IF"),
        neuron_json("output", OUTPUT_UUID, 0.0, "IDENTITY"),
    ];
    let mut synapses = Vec::with_capacity(7);
    push_dual_role_synapses(spec, &mut synapses);
    synapses.push(synapse_json(IF_UUID, OUTPUT_UUID, 1.0));

    creature_envelope(spec.num_inputs, 1, &neurons, &synapses)
}

/// The same function written the pre-#577 way: every repeated source split out
/// behind an extra neuron.
///
/// Three constants replace the one, and two `IDENTITY` relays replace the
/// shared column's direct branch edges. No ordered pair repeats, so this form
/// was — and remains — legal under the old `(from, to)` key. It exists to be
/// scored alongside [`dual_role_if_creature_json`]: the two must agree exactly.
///
/// Relay arithmetic is `0.0 + 1.0 * x`, and each `IF` bucket sums in the same
/// order as the relay-free form, so the equality is bit-exact and not a
/// tolerance.
#[must_use]
pub fn relay_equivalent_if_creature_json(spec: &TreeSpec) -> String {
    let shared = format!("input-{}", shared_feature(spec));
    let neurons = vec![
        constant_neuron_json(CONST_ONE_UUID, 1.0),
        constant_neuron_json(CONST_ONE_POSITIVE_UUID, 1.0),
        constant_neuron_json(CONST_ONE_NEGATIVE_UUID, 1.0),
        neuron_json("hidden", RELAY_POSITIVE_UUID, 0.0, "IDENTITY"),
        neuron_json("hidden", RELAY_NEGATIVE_UUID, 0.0, "IDENTITY"),
        neuron_json("hidden", IF_UUID, 0.0, "IF"),
        neuron_json("output", OUTPUT_UUID, 0.0, "IDENTITY"),
    ];
    let mut synapses = vec![
        synapse_json(&shared, RELAY_POSITIVE_UUID, 1.0),
        synapse_json(&shared, RELAY_NEGATIVE_UUID, 1.0),
        typed_synapse_json(
            &format!("input-{}", condition_feature(spec)),
            IF_UUID,
            1.0,
            Some("condition"),
        ),
        typed_synapse_json(
            CONST_ONE_UUID,
            IF_UUID,
            -spec.threshold(0),
            Some("condition"),
        ),
        typed_synapse_json(
            CONST_ONE_POSITIVE_UUID,
            IF_UUID,
            spec.leaf_value(1),
            Some("positive"),
        ),
        typed_synapse_json(
            CONST_ONE_NEGATIVE_UUID,
            IF_UUID,
            spec.leaf_value(0),
            Some("negative"),
        ),
        typed_synapse_json(
            RELAY_POSITIVE_UUID,
            IF_UUID,
            SHARED_BRANCH_WEIGHT,
            Some("positive"),
        ),
        typed_synapse_json(
            RELAY_NEGATIVE_UUID,
            IF_UUID,
            SHARED_BRANCH_WEIGHT,
            Some("negative"),
        ),
    ];
    synapses.push(synapse_json(IF_UUID, OUTPUT_UUID, 1.0));

    creature_envelope(spec.num_inputs, 1, &neurons, &synapses)
}

/// [`dual_role_if_creature_json`] with the shared source's **negative** edge
/// removed — what a `(from, to)`-keyed loader is left holding.
///
/// Measured against NEAT-AI 6.6.39, loading the relay-free creature keeps only
/// the first synapse of each repeated pair, so the shared contribution survives
/// on one branch and vanishes on the other. Scoring this against the intact
/// creature is how a test proves the dropped edge changes the answer instead of
/// assuming it does.
#[must_use]
pub fn dropped_shared_branch_creature_json(spec: &TreeSpec) -> String {
    let dropped = typed_synapse_json(
        &format!("input-{}", shared_feature(spec)),
        IF_UUID,
        SHARED_BRANCH_WEIGHT,
        Some("negative"),
    );
    let intact = dual_role_if_creature_json(spec);
    let stripped = intact.replacen(&format!("{dropped},"), "", 1);
    assert_ne!(
        stripped, intact,
        "the negative branch edge must be present to be dropped"
    );
    stripped
}

/// A creature whose repeated pair targets a **non-`IF`** neuron.
///
/// Every other squash sums its inward synapses regardless of role, so two from
/// one source are exactly one with the summed weight — `neat-core` rejects this
/// with `TypedDuplicateSynapse` (Issue #577). The fixture pins that the relaxed
/// rule stayed narrow.
#[must_use]
pub fn dual_role_into_pointwise_creature_json(spec: &TreeSpec) -> String {
    let neurons = vec![
        neuron_json("hidden", "hidden-0", 0.0, "TANH"),
        neuron_json("output", OUTPUT_UUID, 0.0, "IDENTITY"),
    ];
    let shared = format!("input-{}", shared_feature(spec));
    let synapses = vec![
        typed_synapse_json(&shared, "hidden-0", SHARED_BRANCH_WEIGHT, Some("positive")),
        typed_synapse_json(&shared, "hidden-0", SHARED_BRANCH_WEIGHT, Some("negative")),
        synapse_json("hidden-0", OUTPUT_UUID, 1.0),
    ];

    creature_envelope(spec.num_inputs, 1, &neurons, &synapses)
}

/// A creature repeating one exact `(from, to, type)` triple.
///
/// The triple is the whole key, so this stays a `DuplicateSynapse` rejection
/// even for an `IF` target: relaxing the pair did not relax the triple.
#[must_use]
pub fn repeated_triple_creature_json(spec: &TreeSpec) -> String {
    let repeated = typed_synapse_json(
        &format!("input-{}", shared_feature(spec)),
        IF_UUID,
        SHARED_BRANCH_WEIGHT,
        Some("positive"),
    );
    let intact = dual_role_if_creature_json(spec);
    let doubled = intact.replacen(&repeated, &format!("{repeated},{repeated}"), 1);
    assert_ne!(
        doubled, intact,
        "the positive edge must exist to be doubled"
    );
    doubled
}

/// Independent reference evaluation of the relay-free creature for one record.
///
/// Written from the decision semantics — descend positive when the condition sum
/// is **strictly** greater than zero — and summing each bucket in the fixture's
/// emission order, so the result is bit-identical to the CPU pipeline's `f32`
/// activation.
///
/// # Panics
///
/// Panics if `inputs` is shorter than `spec.num_inputs`.
#[must_use]
pub fn dual_role_reference_output(spec: &TreeSpec, inputs: &[f32]) -> f32 {
    assert!(
        inputs.len() >= spec.num_inputs,
        "reference evaluation needs {} inputs, got {}",
        spec.num_inputs,
        inputs.len()
    );
    let condition =
        inputs[condition_feature(spec)] * 1.0f32 + 1.0f32 * ((-spec.threshold(0)) as f32);
    let shared = inputs[shared_feature(spec)] * (SHARED_BRANCH_WEIGHT as f32);
    let branch = if condition > 0.0 {
        1.0f32 * (spec.leaf_value(1) as f32)
    } else {
        1.0f32 * (spec.leaf_value(0) as f32)
    };
    branch + shared
}

/// A packed `inputs || target` corpus for the dual-role creature.
///
/// Targets come from a **different** seed so the loss is non-degenerate: a
/// zero-loss corpus would make a relative-error comparison vacuous and would
/// hide a dropped branch edge behind a floor.
#[must_use]
pub fn dual_role_corpus_records(spec: &TreeSpec, num_records: usize) -> Vec<f32> {
    let oracle = TreeSpec::new(spec.num_inputs, 1, spec.seed ^ 0x5A5A_5A5A);
    let mut out = Vec::with_capacity(num_records * (spec.num_inputs + 1));
    for r in 0..num_records {
        let inputs = varied_inputs(spec.num_inputs, r);
        let target = dual_role_reference_output(&oracle, &inputs);
        out.extend_from_slice(&inputs);
        out.push(target);
    }
    out
}

/// A packed corpus pinning the condition to its boundary — exactly on the
/// threshold, one ULP below and one ULP above — so `condition == 0` (which must
/// take the negative branch) is scored alongside its neighbours.
#[must_use]
pub fn dual_role_boundary_records(spec: &TreeSpec) -> Vec<f32> {
    let feature = condition_feature(spec);
    let threshold = spec.threshold(0) as f32;
    let mut out = Vec::new();
    for (i, x) in [threshold, threshold.next_down(), threshold.next_up()]
        .into_iter()
        .enumerate()
    {
        let mut inputs = varied_inputs(spec.num_inputs, i);
        inputs[feature] = x;
        let target = dual_role_reference_output(spec, &inputs);
        out.extend_from_slice(&inputs);
        out.push(target);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use neat_core::creature::{compile_creature, parse_creature_json};
    use neat_core::synapse_type::SynapseType;

    const SPEC: TreeSpec = TreeSpec::new(4, 1, 4_242);

    #[test]
    fn the_shared_column_is_not_the_condition_column() {
        assert_ne!(condition_feature(&SPEC), shared_feature(&SPEC));
        // Edge case: a one-column creature has nowhere else to go, and the
        // shared term legitimately lands on the condition column.
        let narrow = TreeSpec::new(1, 1, 7);
        assert_eq!(condition_feature(&narrow), shared_feature(&narrow));
    }

    #[test]
    fn the_relay_free_creature_keeps_every_repeated_pair() {
        let creature = parse_creature_json(&dual_role_if_creature_json(&SPEC)).expect("parse");
        assert_eq!(creature.synapses.len(), 7);
        let net = compile_creature(&creature).expect("compile");
        assert_eq!(
            net.synapses.len(),
            7,
            "compiling must drop no synapse — a (from, to)-keyed loader keeps only 5"
        );
    }

    #[test]
    fn the_if_neuron_carries_both_branch_roles_from_one_source() {
        let creature = parse_creature_json(&dual_role_if_creature_json(&SPEC)).expect("parse");
        let net = compile_creature(&creature).expect("compile");
        let node = net
            .neurons
            .iter()
            .find(|n| !n.is_constant && n.num_synapses == 6)
            .expect("the IF node reads six synapses");
        let start = node.start_synapse as usize;
        let roles: Vec<SynapseType> = net.synapses[start..start + 6]
            .iter()
            .map(|s| SynapseType::from(s.synapse_type))
            .collect();
        assert_eq!(
            roles,
            vec![
                SynapseType::Condition,
                SynapseType::Condition,
                SynapseType::Positive,
                SynapseType::Negative,
                SynapseType::Positive,
                SynapseType::Negative,
            ]
        );
    }

    #[test]
    fn both_forms_compile_to_the_same_synapse_roles_on_the_if_node() {
        for json in [
            dual_role_if_creature_json(&SPEC),
            relay_equivalent_if_creature_json(&SPEC),
        ] {
            let creature = parse_creature_json(&json).expect("parse");
            compile_creature(&creature).expect("compile");
        }
    }

    #[test]
    fn the_dropped_variant_loses_exactly_one_synapse() {
        let dropped =
            parse_creature_json(&dropped_shared_branch_creature_json(&SPEC)).expect("parse");
        assert_eq!(dropped.synapses.len(), 6);
        compile_creature(&dropped).expect("the stripped creature is still valid");
    }

    #[test]
    fn a_repeated_triple_is_rejected() {
        let creature = parse_creature_json(&repeated_triple_creature_json(&SPEC)).expect("parse");
        let Err(err) = compile_creature(&creature) else {
            panic!("an exact (from, to, type) repeat must still be refused");
        };
        assert!(
            format!("{err}").contains("Duplicate"),
            "expected a duplicate-synapse refusal, got {err}"
        );
    }

    #[test]
    fn a_repeated_pair_into_a_pointwise_target_is_rejected() {
        let creature =
            parse_creature_json(&dual_role_into_pointwise_creature_json(&SPEC)).expect("parse");
        let Err(err) = compile_creature(&creature) else {
            panic!("only an IF target may read one source through two roles");
        };
        let msg = format!("{err}");
        assert!(
            msg.contains("role") || msg.contains("Duplicate"),
            "expected a typed-duplicate refusal, got {msg}"
        );
    }

    #[test]
    fn the_reference_takes_the_negative_branch_when_the_condition_is_zero() {
        let feature = condition_feature(&SPEC);
        let threshold = SPEC.threshold(0) as f32;
        let mut inputs = varied_inputs(SPEC.num_inputs, 0);
        let shared = inputs[shared_feature(&SPEC)] * (SHARED_BRANCH_WEIGHT as f32);

        inputs[feature] = threshold;
        assert_eq!(
            dual_role_reference_output(&SPEC, &inputs),
            (SPEC.leaf_value(0) as f32) + shared,
            "condition == 0 must take the negative branch, shared term included"
        );
        inputs[feature] = threshold.next_up();
        assert_eq!(
            dual_role_reference_output(&SPEC, &inputs),
            (SPEC.leaf_value(1) as f32) + shared,
            "condition > 0 must take the positive branch, shared term included"
        );
    }

    #[test]
    fn the_boundary_corpus_covers_the_node_three_times() {
        let records = dual_role_boundary_records(&SPEC);
        assert_eq!(records.len(), 3 * (SPEC.num_inputs + 1));
    }

    #[test]
    fn the_corpus_targets_do_not_match_the_creature_exactly() {
        // A zero-loss corpus would make the parity comparisons vacuous.
        let records = dual_role_corpus_records(&SPEC, 32);
        let width = SPEC.num_inputs + 1;
        let differing = (0..32)
            .filter(|r| {
                let base = r * width;
                let inputs = &records[base..base + SPEC.num_inputs];
                records[base + SPEC.num_inputs] != dual_role_reference_output(&SPEC, inputs)
            })
            .count();
        assert!(
            differing > 0,
            "every target reproduced the creature exactly"
        );
    }
}
