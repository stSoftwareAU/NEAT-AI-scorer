//! Shallow-creature benchmark fixture — Issue #467.
//!
//! The [`production`](crate::prod_fixture) fixture gates optimisations against
//! the deep evolved creature (~1666 hidden neurons). Issue #467 asks a
//! different question: does the GPU help a **shallow** creature — the
//! `Enceladus` island shape (2461 inputs → **16** hidden → 1 output, ≈ 12 180
//! synapses, all point-wise squashes)? Both shapes exceed the 256-neuron
//! private-kernel cap (inputs count toward it), so both route to the
//! `forward_mse_scratch` GPU kernel; the shallow shape simply does far less
//! per-record work, which is exactly the regime the A/B interrogates.
//!
//! The real `Enceladus` creature lives in a private sample repository and is
//! **never** committed or fetched here (mirroring the [`prod_fixture`](crate::prod_fixture)
//! contract).
//! This module builds a **synthetic** stand-in of the same topology so the
//! CPU-vs-GPU result is reproducible in CI without the private creature. The
//! pure functions below are unit-tested; the Criterion `shallow_gpu_vs_cpu`
//! group in [`benches/scoring.rs`](../../benches/scoring.rs) consumes them.

use crate::fixture_json::{creature_envelope, neuron_json, synapse_json};

/// The point-wise squash mix observed in the real `Enceladus` hidden layer.
///
/// Every entry is a **point-wise** activation (`SquashType` 0..=31), so the
/// synthetic creature is fully GPU-hostable by both scratch and batched kernels
/// — the same property the real creature has. Cycling this set across the
/// hidden neurons reproduces the transcendental mix (`Mish`, `SELU`, `ELU`, …)
/// that determines per-neuron CPU cost, the variable the A/B is sensitive to.
pub(crate) const ENCELADUS_SQUASHES: &[&str] = &[
    "LogSigmoid",
    "LeakyReLU",
    "Softplus",
    "ReLU",
    "ELU",
    "BENT_IDENTITY",
    "SELU",
    "ABSOLUTE",
    "ReLU6",
    "Mish",
    "SOFTSIGN",
    "ArcTan",
    "TANH",
];

/// How many `input → hidden` and `hidden → output` synapses a shallow creature
/// of the given shape carries for a requested `target_synapses` total.
///
/// The `hidden → output` block is always fully wired (`hidden * num_outputs`);
/// the remainder is spent on `input → hidden` edges. The `input → hidden` count
/// is clamped to `[hidden, num_inputs * hidden]` so every hidden neuron has at
/// least one incoming edge (no degenerate constant activation) and the wiring
/// never exceeds the fully-connected maximum.
///
/// Returns `(input_hidden, hidden_output)`. The realised total
/// (`input_hidden + hidden_output`) can differ from `target_synapses` only when
/// the request is outside the representable range; callers that need the exact
/// figure should sum the pair.
// Issue #470 vestigial-parameter sweep: every in-tree caller passes
// `num_outputs = 1` (the Enceladus shape), but the parameter is genuinely read
// — it sets the `hidden → output` edge count — and keeping it generic keeps
// this fixture symmetrical with `prod_fixture`. Not a vestige.
#[must_use]
pub(crate) fn plan_synapses(
    num_inputs: usize,
    num_outputs: usize,
    hidden: usize,
    target_synapses: usize,
) -> (usize, usize) {
    let hidden_output = hidden.saturating_mul(num_outputs);
    let remainder = target_synapses.saturating_sub(hidden_output);
    let max_input_hidden = num_inputs.saturating_mul(hidden);
    let min_input_hidden = hidden.min(max_input_hidden);
    let input_hidden = remainder.clamp(min_input_hidden, max_input_hidden);
    (input_hidden, hidden_output)
}

/// Build a forward-only synthetic creature JSON matching the shallow `Enceladus`
/// topology.
///
/// `input → hidden` edges are laid out as `(input = c / hidden, hidden =
/// c % hidden)` for `c` in `0..input_hidden`, which yields **distinct** pairs
/// (a base-`hidden` decomposition) spread evenly across the hidden layer while
/// keeping the input index within `num_inputs`. Hidden neurons cycle
/// `ENCELADUS_SQUASHES`; outputs use `IDENTITY`. The result parses with
/// `neat_core::creature::parse_creature_json` and reports exactly
/// `input_hidden + hidden_output` synapses (see `plan_synapses`).
#[must_use]
pub fn shallow_creature_json(
    num_inputs: usize,
    num_outputs: usize,
    hidden: usize,
    target_synapses: usize,
) -> String {
    let (input_hidden, _hidden_output) =
        plan_synapses(num_inputs, num_outputs, hidden, target_synapses);

    let mut neurons: Vec<String> = Vec::with_capacity(hidden + num_outputs);
    for h in 0..hidden {
        let squash = ENCELADUS_SQUASHES[h % ENCELADUS_SQUASHES.len()];
        neurons.push(neuron_json("hidden", &format!("hidden-{h}"), 0.05, squash));
    }
    for o in 0..num_outputs {
        neurons.push(neuron_json(
            "output",
            &format!("output-{o}"),
            0.0,
            "IDENTITY",
        ));
    }

    let mut synapses: Vec<String> = Vec::with_capacity(input_hidden + hidden * num_outputs);
    // input -> hidden, distinct (input, hidden) pairs spread across the layer.
    // Weights are small and sign-alternating so the pre-activation of a hidden
    // neuron fed by hundreds of inputs stays bounded: the *unbounded* squashes
    // in the Enceladus mix (Softplus/ELU/Mish/ReLU) would overflow f32 to `inf`
    // under large weights, whereas the real creature carries trained small
    // weights. Timing depends on topology and squash type, not weight
    // magnitude, so this keeps the A/B representative while finite.
    for c in 0..input_hidden {
        let i = c.checked_div(hidden).unwrap_or(0);
        let h = c.checked_rem(hidden).unwrap_or(0);
        let sign = if c % 2 == 0 { 1.0 } else { -1.0 };
        let w = sign * (0.002 + 0.0001 * ((c % 37) as f64));
        synapses.push(synapse_json(
            &format!("input-{i}"),
            &format!("hidden-{h}"),
            w,
        ));
    }
    // hidden -> output, fully wired.
    for h in 0..hidden {
        for o in 0..num_outputs {
            let w = 0.05 + 0.001 * ((h * num_outputs + o) as f64);
            synapses.push(synapse_json(
                &format!("hidden-{h}"),
                &format!("output-{o}"),
                w,
            ));
        }
    }

    creature_envelope(num_inputs, num_outputs, &neurons, &synapses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use neat_core::creature::parse_creature_json;

    // The real Enceladus shape, observed in the private sample repository.
    const ENC_INPUTS: usize = 2461;
    const ENC_HIDDEN: usize = 16;
    const ENC_SYNAPSES: usize = 12_171;

    #[test]
    fn plan_synapses_hits_enceladus_shape() {
        // Happy path: 16*1 = 16 hidden->output, rest input->hidden.
        let (ih, ho) = plan_synapses(ENC_INPUTS, 1, ENC_HIDDEN, ENC_SYNAPSES);
        assert_eq!(ho, 16);
        assert_eq!(ih, ENC_SYNAPSES - 16);
        assert_eq!(ih + ho, ENC_SYNAPSES);
    }

    #[test]
    fn plan_synapses_clamps_below_minimum() {
        // Edge case: a target smaller than the hidden->output block still
        // guarantees one incoming edge per hidden neuron.
        let (ih, ho) = plan_synapses(ENC_INPUTS, 1, ENC_HIDDEN, 4);
        assert_eq!(ho, 16);
        assert_eq!(ih, ENC_HIDDEN, "input->hidden clamps up to `hidden`");
    }

    #[test]
    fn plan_synapses_clamps_to_fully_connected_maximum() {
        // Edge case: an absurd target cannot exceed inputs*hidden.
        let (ih, _ho) = plan_synapses(10, 1, 4, 1_000_000);
        assert_eq!(ih, 40, "input->hidden clamps down to num_inputs*hidden");
    }

    #[test]
    fn shallow_creature_json_parses_with_expected_topology() {
        let json = shallow_creature_json(ENC_INPUTS, 1, ENC_HIDDEN, ENC_SYNAPSES);
        let creature = parse_creature_json(&json).expect("shallow creature parses");
        assert_eq!(creature.input, ENC_INPUTS);
        assert_eq!(creature.output, 1);
        assert!(creature.forward_only);
        assert_eq!(creature.neurons.len(), ENC_HIDDEN + 1, "hidden + output");
        assert_eq!(
            creature.synapses.len(),
            ENC_SYNAPSES,
            "realised synapse count matches the requested Enceladus total"
        );
    }

    #[test]
    fn shallow_creature_input_hidden_edges_are_distinct() {
        // Regression guard: duplicate (input, hidden) pairs would let neat_core
        // merge edges and silently shrink the synapse count, corrupting the A/B.
        let json = shallow_creature_json(ENC_INPUTS, 1, ENC_HIDDEN, ENC_SYNAPSES);
        let creature = parse_creature_json(&json).expect("parse");
        let mut seen = std::collections::HashSet::new();
        for syn in &creature.synapses {
            assert!(
                seen.insert((syn.from_uuid.clone(), syn.to_uuid.clone())),
                "duplicate synapse {} -> {}",
                syn.from_uuid,
                syn.to_uuid
            );
        }
    }

    #[test]
    fn shallow_creature_uses_only_pointwise_squashes() {
        // Every hidden squash must be a real, GPU-hostable point-wise name so
        // the synthetic creature routes through the same kernel as the real one.
        let json = shallow_creature_json(ENC_INPUTS, 1, ENC_HIDDEN, ENC_SYNAPSES);
        let creature = parse_creature_json(&json).expect("parse");
        let hidden: Vec<_> = creature
            .neurons
            .iter()
            .filter(|n| n.neuron_type == "hidden")
            .collect();
        assert_eq!(hidden.len(), ENC_HIDDEN);
        for n in hidden {
            let squash = n.squash.as_deref().expect("hidden neuron has a squash");
            assert!(
                ENCELADUS_SQUASHES.contains(&squash),
                "unexpected hidden squash {squash}"
            );
        }
    }
}
