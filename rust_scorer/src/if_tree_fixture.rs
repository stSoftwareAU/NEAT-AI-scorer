//! Canonical `IF` decision-tree fixtures — Issue #574.
//!
//! `NEAT-AI-Forests` will generate tree-shaped creatures built from
//! [`SquashType::If`](neat_core::squash::SquashType) plus the
//! `Condition` / `Negative` / `Positive` synapse roles, and will trust this
//! scorer as the final judge. That makes the branch semantics a **contract**:
//! the CPU pipeline and both GPU kernels must pick the same branch for the same
//! record, including at the `condition == 0` boundary, and an `IF` neuron must
//! never be reinterpreted as an ordinary point-wise squash.
//!
//! This module builds the fixtures that lock that contract. Every creature is
//! emitted through [`crate::fixture_json`] (the single authoritative wire-format
//! emitter) and every fixture is paired with an **independent** reference
//! evaluator — [`tree_reference_output`] — written directly from the decision
//! semantics rather than from the scorer's own code, so a test can assert the
//! scored activation against a value the scorer did not produce.
//!
//! ```text
//!                  ┌──────────────┐
//!  input-f ──cond──▶              │
//!  const-1 ──cond──▶  IF node k   │──▶ positive branch when Σcond  > 0
//!  low  ────neg────▶ (bias 0.0)   │──▶ negative branch when Σcond <= 0
//!  high ────pos────▶              │
//!                  └──────────────┘
//! ```
//!
//! The threshold is injected by a **constant** neuron holding `1.0`
//! ([`BIAS_ONE_UUID`]): the condition bucket sums `x_f * 1.0` and
//! `1.0 * (-threshold)`, so the node tests `x_f > threshold` exactly. Constant
//! neurons are hosted by both GPU kernels (Issue #312), so the fixture needs no
//! reserved bias column in the corpus and works against any record layout.
//!
//! These fixtures give each of the `IF` node's three bias-1 sources its own
//! constant ([`BIAS_ONE_UUID`], [`BIAS_ONE_POSITIVE_UUID`],
//! [`BIAS_ONE_NEGATIVE_UUID`]) rather than reusing one three times. All three
//! hold the same `1.0`, so the arithmetic is identical either way — the split
//! is about the wiring, and it is now a **conservative choice, not a
//! requirement**: `neat_core::compile_creature` keys synapses by the
//! `(from, to, type)` triple since NEAT-AI-core#577, so an `IF` target may read
//! one source through several roles. The split is kept here because NEAT-AI's
//! TypeScript loader still keys on `(from, to)` alone (NEAT-AI#3873 is open),
//! so these fixtures stay loadable by **both** engines. The relaxed shape has
//! its own fixtures and its own parity guard in
//! [`crate::dual_role_fixture`] / `tests/dual_role_parity.rs` (Issue #581).
//!
//! Once `NEAT-AI-core#555` lands its canonical fixture and graft helper, these
//! builders become the scorer-side consumers of it; the parity assertions in
//! `tests/if_tree_parity.rs` are written against the semantics, not the builder,
//! so the swap is a fixture change rather than a test rewrite.

use crate::fixture_json::{creature_envelope, neuron_json, synapse_json, typed_synapse_json};

/// UUID of the constant neuron every tree fixture uses to inject `1.0` into the
/// **condition** bucket.
pub const BIAS_ONE_UUID: &str = "const-one";

/// UUID of the constant neuron feeding the **positive** branch of an `IF` node
/// whose high child is a leaf. Distinct from [`BIAS_ONE_UUID`] so the node does
/// not carry two synapses from the same source — no longer required of an `IF`
/// target since NEAT-AI-core#577, but kept so the fixture also loads under
/// NEAT-AI's `(from, to)`-keyed TypeScript loader (Issue #581).
pub const BIAS_ONE_POSITIVE_UUID: &str = "const-one-positive";

/// UUID of the constant neuron feeding the **negative** branch of an `IF` node
/// whose low child is a leaf. Distinct from [`BIAS_ONE_UUID`] for the same
/// reason as [`BIAS_ONE_POSITIVE_UUID`].
pub const BIAS_ONE_NEGATIVE_UUID: &str = "const-one-negative";

/// The three bias-1 constants an `IF` node hangs off, in emission order.
///
/// Listed ahead of every hidden neuron that reads them, and all holding the
/// same `1.0` — one per synapse role, so no `(from, to)` pair repeats and the
/// creature loads under either engine's key (see the module docs).
fn bias_one_neurons() -> Vec<String> {
    vec![
        neuron_json("constant", BIAS_ONE_UUID, 1.0, "IDENTITY"),
        neuron_json("constant", BIAS_ONE_POSITIVE_UUID, 1.0, "IDENTITY"),
        neuron_json("constant", BIAS_ONE_NEGATIVE_UUID, 1.0, "IDENTITY"),
    ]
}

/// UUID of the single output neuron every fixture in this module emits.
pub const OUTPUT_UUID: &str = "output-0";

/// A perfect binary decision tree of `IF` neurons.
///
/// Node ids use heap numbering: the root is `0`, and node `k`'s low (negative)
/// and high (positive) children are `2k + 1` and `2k + 2`. Ids below
/// [`num_internal_nodes`](TreeSpec::num_internal_nodes) are internal `IF`
/// neurons; the rest are leaves whose constant values come from
/// [`leaf_value`](TreeSpec::leaf_value).
///
/// Every derived parameter (split feature, threshold, leaf value) is a pure
/// function of `seed` and the node id, so a spec is reproducible across hosts
/// and a whole population of distinct candidates is just a range of seeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeSpec {
    /// Number of input columns the creature reads.
    pub num_inputs: usize,
    /// Tree depth: `1` is a decision stump, `2` a nested depth-2 tree.
    pub depth: u32,
    /// Seed selecting this candidate's split features, thresholds and leaves.
    pub seed: u64,
}

/// Deterministic 64-bit mixer (SplitMix64 finaliser) — keeps fixture parameters
/// reproducible without pulling in an RNG dependency.
const fn mix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl TreeSpec {
    /// A tree of `depth` splits over `num_inputs` input columns.
    #[must_use]
    pub const fn new(num_inputs: usize, depth: u32, seed: u64) -> Self {
        Self {
            num_inputs,
            depth,
            seed,
        }
    }

    /// Internal (`IF`) node count — `2^depth - 1`.
    #[must_use]
    pub const fn num_internal_nodes(&self) -> usize {
        (1usize << self.depth) - 1
    }

    /// Leaf count — `2^depth`.
    #[must_use]
    pub const fn num_leaves(&self) -> usize {
        1usize << self.depth
    }

    /// Input column node `node` splits on.
    #[must_use]
    pub fn feature(&self, node: usize) -> usize {
        let m = mix(self.seed ^ ((node as u64) << 17) ^ 0x01);
        (m % self.num_inputs as u64) as usize
    }

    /// Split threshold for `node`, in `[-0.5, 0.5]` so records drawn from
    /// `[-1, 1]` exercise both branches.
    #[must_use]
    pub fn threshold(&self, node: usize) -> f64 {
        let m = mix(self.seed ^ ((node as u64) << 17) ^ 0x02);
        (m % 1001) as f64 / 1000.0 - 0.5
    }

    /// Constant value of leaf `leaf` (`0 .. num_leaves`), in `[-1, 1]`.
    #[must_use]
    pub fn leaf_value(&self, leaf: usize) -> f64 {
        let m = mix(self.seed ^ ((leaf as u64) << 23) ^ 0x03);
        (m % 2001) as f64 / 1000.0 - 1.0
    }

    /// Every leaf value, in leaf-index order.
    #[must_use]
    pub fn leaf_values(&self) -> Vec<f32> {
        (0..self.num_leaves())
            .map(|l| self.leaf_value(l) as f32)
            .collect()
    }
}

/// UUID of internal `IF` node `id`.
#[must_use]
pub fn node_uuid(id: usize) -> String {
    format!("if-{id}")
}

/// Emit the condition + branch synapses of internal node `id` into `synapses`.
///
/// Emission order per node is fixed — `condition(feature)`,
/// `condition(-threshold)`, `positive`, `negative` — because `compile_creature`
/// preserves it, and the reference evaluator sums the condition bucket in the
/// same order. That makes the reference **bit-exact** against the CPU pipeline
/// rather than merely close.
fn push_node_synapses(spec: &TreeSpec, id: usize, synapses: &mut Vec<String>) {
    let internal = spec.num_internal_nodes();
    let to = node_uuid(id);
    synapses.push(typed_synapse_json(
        &format!("input-{}", spec.feature(id)),
        &to,
        1.0,
        Some("condition"),
    ));
    synapses.push(typed_synapse_json(
        BIAS_ONE_UUID,
        &to,
        -spec.threshold(id),
        Some("condition"),
    ));
    for (child, role, leaf_one) in [
        (2 * id + 2, "positive", BIAS_ONE_POSITIVE_UUID),
        (2 * id + 1, "negative", BIAS_ONE_NEGATIVE_UUID),
    ] {
        if child < internal {
            synapses.push(typed_synapse_json(&node_uuid(child), &to, 1.0, Some(role)));
        } else {
            synapses.push(typed_synapse_json(
                leaf_one,
                &to,
                spec.leaf_value(child - internal),
                Some(role),
            ));
        }
    }
}

/// Build the decision-tree creature described by `spec`.
///
/// Neurons are emitted in topological order — the constant, then the `IF` nodes
/// deepest-level first, then the single `IDENTITY` output that passes the root
/// activation through unchanged. A `depth = 1` spec is the canonical
/// `x > threshold` stump; `depth = 2` the nested tree.
#[must_use]
pub fn tree_creature_json(spec: &TreeSpec) -> String {
    let internal = spec.num_internal_nodes();
    let mut neurons = bias_one_neurons();
    let mut synapses = Vec::with_capacity(internal * 4 + 1);
    // Heap numbering is level order, so descending ids visit the deepest level
    // first — every child is emitted before the parent that reads it.
    for id in (0..internal).rev() {
        neurons.push(neuron_json("hidden", &node_uuid(id), 0.0, "IF"));
        push_node_synapses(spec, id, &mut synapses);
    }
    neurons.push(neuron_json("output", OUTPUT_UUID, 0.0, "IDENTITY"));
    synapses.push(synapse_json(&node_uuid(0), OUTPUT_UUID, 1.0));

    creature_envelope(spec.num_inputs, 1, &neurons, &synapses)
}

/// Independent reference evaluation of `spec` for one record's inputs.
///
/// Written from the decision semantics — descend high when the condition sum is
/// **strictly** greater than zero, low otherwise — not from the scorer's
/// activation code. The arithmetic mirrors the emitted synapse order exactly, so
/// the result is bit-identical to the CPU pipeline's `f32` activation.
///
/// # Panics
///
/// Panics if `inputs` is shorter than `spec.num_inputs`.
#[must_use]
pub fn tree_reference_output(spec: &TreeSpec, inputs: &[f32]) -> f32 {
    assert!(
        inputs.len() >= spec.num_inputs,
        "reference evaluation needs {} inputs, got {}",
        spec.num_inputs,
        inputs.len()
    );
    let internal = spec.num_internal_nodes();
    let mut node = 0usize;
    loop {
        let condition =
            inputs[spec.feature(node)] * 1.0f32 + 1.0f32 * ((-spec.threshold(node)) as f32);
        let child = if condition > 0.0 {
            2 * node + 2
        } else {
            2 * node + 1
        };
        if child < internal {
            node = child;
        } else {
            return spec.leaf_value(child - internal) as f32;
        }
    }
}

/// A creature mixing ordinary point-wise neurons with an `IF` node.
///
/// `hidden` `TANH` neurons read every input and feed the **condition** bucket of
/// a single `IF` node whose branches are the constants of a depth-1 `spec`. The
/// prediction is therefore still exactly one of the two leaf values — an
/// invariant a point-wise reinterpretation of `IF` cannot satisfy — while the
/// forward pass exercises a real non-linear layer ahead of the branch.
#[must_use]
pub fn mixed_neural_if_creature_json(spec: &TreeSpec, hidden: usize) -> String {
    let num_inputs = spec.num_inputs;
    let mut neurons = Vec::with_capacity(hidden + 5);
    for h in 0..hidden {
        neurons.push(neuron_json("hidden", &format!("hidden-{h}"), 0.05, "TANH"));
    }
    neurons.extend(bias_one_neurons());
    neurons.push(neuron_json("hidden", &node_uuid(0), 0.0, "IF"));
    neurons.push(neuron_json("output", OUTPUT_UUID, 0.0, "IDENTITY"));

    let mut synapses = Vec::with_capacity(num_inputs * hidden + hidden + 3);
    for i in 0..num_inputs {
        for h in 0..hidden {
            let w = 0.05 + 0.01 * ((i * hidden + h) as f64);
            synapses.push(synapse_json(
                &format!("input-{i}"),
                &format!("hidden-{h}"),
                w,
            ));
        }
    }
    let root = node_uuid(0);
    for h in 0..hidden {
        // Alternating signs keep the condition sum crossing zero across the
        // corpus instead of saturating on one branch.
        let w = if h % 2 == 0 { 0.7 } else { -0.6 };
        synapses.push(typed_synapse_json(
            &format!("hidden-{h}"),
            &root,
            w,
            Some("condition"),
        ));
    }
    synapses.push(typed_synapse_json(
        BIAS_ONE_UUID,
        &root,
        -spec.threshold(0),
        Some("condition"),
    ));
    synapses.push(typed_synapse_json(
        BIAS_ONE_POSITIVE_UUID,
        &root,
        spec.leaf_value(1),
        Some("positive"),
    ));
    synapses.push(typed_synapse_json(
        BIAS_ONE_NEGATIVE_UUID,
        &root,
        spec.leaf_value(0),
        Some("negative"),
    ));
    synapses.push(synapse_json(&root, OUTPUT_UUID, 1.0));

    creature_envelope(num_inputs, 1, &neurons, &synapses)
}

/// A large point-wise creature carrying a small appended `IF` correction graft.
///
/// This is the Forests shape that matters for routing: a `hidden`-wide `TANH`
/// layer (push `hidden` above the private kernel's 256-neuron cap to reach the
/// `forward_mse_scratch` kernel) whose summed output is corrected by a depth-1
/// `IF` node reading one input column. The graft contributes a constant per
/// branch, so removing it shifts every prediction — a graft that silently did
/// nothing would be visible.
///
/// The graft is always **depth 1** whatever `spec.depth` says — a correction
/// patch, not a whole tree — so `spec` only contributes the split feature,
/// threshold and the two leaf constants.
#[must_use]
pub fn grafted_creature_json(spec: &TreeSpec, hidden: usize) -> String {
    let spec = &TreeSpec::new(spec.num_inputs, 1, spec.seed);
    let num_inputs = spec.num_inputs;
    let mut neurons = Vec::with_capacity(hidden + 5);
    for h in 0..hidden {
        neurons.push(neuron_json("hidden", &format!("hidden-{h}"), 0.05, "TANH"));
    }
    neurons.extend(bias_one_neurons());
    neurons.push(neuron_json("hidden", &node_uuid(0), 0.0, "IF"));
    neurons.push(neuron_json("output", OUTPUT_UUID, 0.0, "IDENTITY"));

    let mut synapses = Vec::with_capacity(num_inputs * hidden + hidden + 5);
    for i in 0..num_inputs {
        for h in 0..hidden {
            let w = 0.05 + 0.001 * ((i * hidden + h) as f64);
            synapses.push(synapse_json(
                &format!("input-{i}"),
                &format!("hidden-{h}"),
                w,
            ));
        }
    }
    push_node_synapses(spec, 0, &mut synapses);
    for h in 0..hidden {
        let w = 0.1 / (hidden as f64) + 0.0001 * (h as f64);
        synapses.push(synapse_json(&format!("hidden-{h}"), OUTPUT_UUID, w));
    }
    synapses.push(synapse_json(&node_uuid(0), OUTPUT_UUID, 1.0));

    creature_envelope(num_inputs, 1, &neurons, &synapses)
}

/// The same creature as [`grafted_creature_json`] with the `IF` graft removed.
///
/// Scoring both proves the graft is live rather than inert.
#[must_use]
pub fn ungrafted_creature_json(spec: &TreeSpec, hidden: usize) -> String {
    let num_inputs = spec.num_inputs;
    let mut neurons: Vec<String> = (0..hidden)
        .map(|h| neuron_json("hidden", &format!("hidden-{h}"), 0.05, "TANH"))
        .collect();
    neurons.push(neuron_json("output", OUTPUT_UUID, 0.0, "IDENTITY"));

    let mut synapses = Vec::with_capacity(num_inputs * hidden + hidden);
    for i in 0..num_inputs {
        for h in 0..hidden {
            let w = 0.05 + 0.001 * ((i * hidden + h) as f64);
            synapses.push(synapse_json(
                &format!("input-{i}"),
                &format!("hidden-{h}"),
                w,
            ));
        }
    }
    for h in 0..hidden {
        let w = 0.1 / (hidden as f64) + 0.0001 * (h as f64);
        synapses.push(synapse_json(&format!("hidden-{h}"), OUTPUT_UUID, w));
    }

    creature_envelope(num_inputs, 1, &neurons, &synapses)
}

/// Deterministic input vector for record `record`, spread over `[-1, 1]`.
#[must_use]
pub fn varied_inputs(num_inputs: usize, record: usize) -> Vec<f32> {
    (0..num_inputs)
        .map(|k| ((record.wrapping_mul(31) + k * 7) as f32 * 0.017).sin())
        .collect()
}

/// A packed `inputs || target` corpus whose target column is `oracle`'s
/// prediction, so a candidate's loss measures how closely it reproduces the
/// oracle tree.
#[must_use]
pub fn corpus_records(oracle: &TreeSpec, num_records: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(num_records * (oracle.num_inputs + 1));
    for r in 0..num_records {
        let inputs = varied_inputs(oracle.num_inputs, r);
        let target = tree_reference_output(oracle, &inputs);
        out.extend_from_slice(&inputs);
        out.push(target);
    }
    out
}

/// A packed corpus that pins every internal node's condition to its boundary.
///
/// For each internal node the corpus contributes three records whose split
/// column sits exactly **on** the threshold, one ULP below it and one ULP above
/// it, so `condition == 0` (which must take the negative branch) is scored
/// alongside its immediate neighbours. Targets are `spec`'s own predictions.
#[must_use]
pub fn boundary_records(spec: &TreeSpec) -> Vec<f32> {
    let mut out = Vec::new();
    for node in 0..spec.num_internal_nodes() {
        let feature = spec.feature(node);
        let threshold = spec.threshold(node) as f32;
        for (i, x) in [threshold, threshold.next_down(), threshold.next_up()]
            .into_iter()
            .enumerate()
        {
            let mut inputs = varied_inputs(spec.num_inputs, node * 3 + i);
            inputs[feature] = x;
            let target = tree_reference_output(spec, &inputs);
            out.extend_from_slice(&inputs);
            out.push(target);
        }
    }
    out
}

/// Serialise packed `f32` records to the little-endian `.bin` corpus layout.
#[must_use]
pub fn records_to_le_bytes(records: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(records.len() * 4);
    for v in records {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use neat_core::creature::{compile_creature, parse_creature_json};
    use neat_core::squash::SquashType;
    use neat_core::synapse_type::SynapseType;

    #[test]
    fn tree_shape_scales_with_depth() {
        let stump = TreeSpec::new(4, 1, 7);
        assert_eq!(stump.num_internal_nodes(), 1);
        assert_eq!(stump.num_leaves(), 2);
        let deep = TreeSpec::new(4, 3, 7);
        assert_eq!(deep.num_internal_nodes(), 7);
        assert_eq!(deep.num_leaves(), 8);
    }

    #[test]
    fn derived_parameters_are_deterministic_and_in_range() {
        let spec = TreeSpec::new(6, 3, 42);
        for node in 0..spec.num_internal_nodes() {
            assert_eq!(spec.feature(node), spec.feature(node));
            assert!(spec.feature(node) < 6);
            let t = spec.threshold(node);
            assert!((-0.5..=0.5).contains(&t), "threshold {t} out of range");
        }
        for leaf in 0..spec.num_leaves() {
            let v = spec.leaf_value(leaf);
            assert!((-1.0..=1.0).contains(&v), "leaf {v} out of range");
        }
    }

    #[test]
    fn distinct_seeds_produce_distinct_trees() {
        let a = TreeSpec::new(8, 2, 1);
        let b = TreeSpec::new(8, 2, 2);
        assert_ne!(tree_creature_json(&a), tree_creature_json(&b));
    }

    #[test]
    fn tree_creature_compiles_with_if_neurons_and_synapse_roles() {
        let spec = TreeSpec::new(4, 2, 11);
        let creature = parse_creature_json(&tree_creature_json(&spec)).expect("parse");
        let net = compile_creature(&creature).expect("compile");

        let if_neurons = net
            .neurons
            .iter()
            .filter(|n| !n.is_constant && SquashType::from(n.squash_type) == SquashType::If)
            .count();
        assert_eq!(if_neurons, spec.num_internal_nodes());

        // Every IF neuron carries two condition edges plus one positive and one
        // negative branch edge.
        for neuron in &net.neurons {
            if neuron.is_constant || SquashType::from(neuron.squash_type) != SquashType::If {
                continue;
            }
            let start = neuron.start_synapse as usize;
            let end = start + neuron.num_synapses as usize;
            let roles: Vec<SynapseType> = net.synapses[start..end]
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
                ]
            );
        }
    }

    #[test]
    fn reference_output_is_always_a_leaf_value() {
        let spec = TreeSpec::new(5, 3, 99);
        let leaves = spec.leaf_values();
        for r in 0..200 {
            let inputs = varied_inputs(spec.num_inputs, r);
            let out = tree_reference_output(&spec, &inputs);
            assert!(
                leaves.contains(&out),
                "reference output {out} is not one of {leaves:?}"
            );
        }
    }

    #[test]
    fn reference_takes_the_negative_branch_when_condition_is_zero() {
        let spec = TreeSpec::new(3, 1, 5);
        let feature = spec.feature(0);
        let threshold = spec.threshold(0) as f32;
        let mut inputs = varied_inputs(spec.num_inputs, 0);

        inputs[feature] = threshold;
        assert_eq!(
            tree_reference_output(&spec, &inputs),
            spec.leaf_value(0) as f32,
            "condition == 0 must take the negative branch"
        );
        inputs[feature] = threshold.next_up();
        assert_eq!(
            tree_reference_output(&spec, &inputs),
            spec.leaf_value(1) as f32,
            "condition > 0 must take the positive branch"
        );
        inputs[feature] = threshold.next_down();
        assert_eq!(
            tree_reference_output(&spec, &inputs),
            spec.leaf_value(0) as f32,
            "condition < 0 must take the negative branch"
        );
    }

    #[test]
    fn boundary_corpus_covers_every_node_three_times() {
        let spec = TreeSpec::new(4, 2, 13);
        let records = boundary_records(&spec);
        let values_per_record = spec.num_inputs + 1;
        assert_eq!(records.len() % values_per_record, 0);
        assert_eq!(
            records.len() / values_per_record,
            spec.num_internal_nodes() * 3
        );
    }

    #[test]
    fn corpus_targets_are_the_oracle_predictions() {
        let oracle = TreeSpec::new(4, 2, 21);
        let records = corpus_records(&oracle, 16);
        let values_per_record = oracle.num_inputs + 1;
        for r in 0..16 {
            let base = r * values_per_record;
            let inputs = &records[base..base + oracle.num_inputs];
            assert_eq!(
                records[base + oracle.num_inputs],
                tree_reference_output(&oracle, inputs)
            );
        }
    }

    #[test]
    fn mixed_and_grafted_creatures_compile() {
        let spec = TreeSpec::new(6, 1, 3);
        for json in [
            mixed_neural_if_creature_json(&spec, 4),
            grafted_creature_json(&spec, 8),
            ungrafted_creature_json(&spec, 8),
        ] {
            let creature = parse_creature_json(&json).expect("parse");
            compile_creature(&creature).expect("compile");
        }
    }

    /// The graft is a depth-1 correction patch: a deep `spec` must not make it
    /// emit edges from tree nodes the grafted creature never carries.
    #[test]
    fn graft_stays_depth_1_for_a_deep_spec() {
        let spec = TreeSpec::new(5, 3, 8);
        for json in [
            mixed_neural_if_creature_json(&spec, 4),
            grafted_creature_json(&spec, 6),
        ] {
            let creature = parse_creature_json(&json).expect("parse");
            let net = compile_creature(&creature).expect("a deep spec must still graft cleanly");
            let if_neurons = net
                .neurons
                .iter()
                .filter(|n| !n.is_constant && SquashType::from(n.squash_type) == SquashType::If)
                .count();
            assert_eq!(if_neurons, 1, "the graft must be a single IF node");
        }
    }

    #[test]
    fn records_serialise_little_endian() {
        let bytes = records_to_le_bytes(&[1.0f32, -2.5f32]);
        assert_eq!(bytes.len(), 8);
        assert_eq!(&bytes[0..4], &1.0f32.to_le_bytes());
        assert_eq!(&bytes[4..8], &(-2.5f32).to_le_bytes());
    }
}
