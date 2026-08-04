//! The single authoritative emitter for the upstream **creature JSON wire
//! format** — Issue #513.
//!
//! Before this module the envelope
//! (`{"input":…,"output":…,"forwardOnly":true,"semanticVersion":"4.0.0",…}`)
//! plus the per-neuron and per-synapse literal shapes were hand-rolled with
//! `format!` in fifteen benches, binaries, tests and fixture modules. A schema
//! change upstream — a `semanticVersion` bump, a renamed field, a new mandatory
//! key — meant the same edit fifteen times with nothing to enforce agreement.
//!
//! Only the **emission** lives here. Callers keep their own loops, shapes and
//! weight formulas: those differ on purpose (each parity test needs distinct
//! magnitudes) and are correct to keep local.
//!
//! The one exception is [`dense_mlp_creature_json`], the
//! `inputs → hidden → outputs` builder that was byte-identical across the GPU
//! parity tests; it is a plain parameterised function, not a per-caller switch.

/// Render a JSON number so an integral `f64` keeps its `.0` suffix.
///
/// `f64`'s `Display` prints `0.0` as `0`. Both parse identically, but the
/// hand-written literals this module replaces all carried the fractional form,
/// so preserving it keeps the emitted bytes unchanged.
fn json_number(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

/// One neuron object: `{"type":…,"uuid":…,"bias":…,"squash":…}`.
///
/// `kind` is the upstream neuron type (`"hidden"`, `"output"`, `"constant"`).
#[must_use]
pub fn neuron_json(kind: &str, uuid: &str, bias: f64, squash: &str) -> String {
    let bias = json_number(bias);
    format!(r#"{{"type":"{kind}","uuid":"{uuid}","bias":{bias},"squash":"{squash}"}}"#)
}

/// One synapse object: `{"fromUUID":…,"toUUID":…,"weight":…}`.
#[must_use]
pub fn synapse_json(from_uuid: &str, to_uuid: &str, weight: f64) -> String {
    typed_synapse_json(from_uuid, to_uuid, weight, None)
}

/// One synapse object carrying an optional aggregate-input `"type"` field
/// (`"condition"`, `"negative"`, `"positive"`), used by `MINIMUM`/`MAXIMUM`/`IF`
/// neurons. `None` emits the plain three-field shape.
#[must_use]
pub fn typed_synapse_json(
    from_uuid: &str,
    to_uuid: &str,
    weight: f64,
    synapse_type: Option<&str>,
) -> String {
    let weight = json_number(weight);
    let ty = synapse_type.map_or_else(String::new, |t| format!(r#","type":"{t}""#));
    format!(r#"{{"fromUUID":"{from_uuid}","toUUID":"{to_uuid}","weight":{weight}{ty}}}"#)
}

/// Wrap pre-rendered `neurons` and `synapses` in the forward-only creature
/// envelope, stamping the `semanticVersion` the scorer supports.
#[must_use]
pub fn creature_envelope(
    inputs: usize,
    outputs: usize,
    neurons: &[String],
    synapses: &[String],
) -> String {
    format!(
        r#"{{"input":{inputs},"output":{outputs},"forwardOnly":true,"semanticVersion":"4.0.0","neurons":[{}],"synapses":[{}]}}"#,
        neurons.join(","),
        synapses.join(","),
    )
}

/// A fully-connected forward-only `num_inputs → hidden → num_outputs` MLP.
///
/// Hidden neurons carry bias `0.05` and `hidden_squash`; outputs carry bias
/// `0.0` and `IDENTITY`. Weights vary mildly with position
/// (`0.05 + 0.001 * (i * hidden + h)` into the hidden layer,
/// `0.1 + 0.001 * (h * num_outputs + o)` out of it) so activations are
/// non-degenerate. This is the exact shape the GPU parity tests, the allocation
/// bench and the batched-kernel unit tests each used to hand-roll.
#[must_use]
pub fn dense_mlp_creature_json(
    num_inputs: usize,
    num_outputs: usize,
    hidden: usize,
    hidden_squash: &str,
) -> String {
    let mut neurons: Vec<String> = Vec::with_capacity(hidden + num_outputs);
    for h in 0..hidden {
        neurons.push(neuron_json(
            "hidden",
            &format!("hidden-{h}"),
            0.05,
            hidden_squash,
        ));
    }
    for o in 0..num_outputs {
        neurons.push(neuron_json(
            "output",
            &format!("output-{o}"),
            0.0,
            "IDENTITY",
        ));
    }

    let mut synapses: Vec<String> = Vec::with_capacity(num_inputs * hidden + hidden * num_outputs);
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
        for o in 0..num_outputs {
            let w = 0.1 + 0.001 * ((h * num_outputs + o) as f64);
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

    #[test]
    fn neuron_json_emits_the_wire_shape() {
        assert_eq!(
            neuron_json("hidden", "hidden-3", 0.05, "TANH"),
            r#"{"type":"hidden","uuid":"hidden-3","bias":0.05,"squash":"TANH"}"#
        );
    }

    #[test]
    fn neuron_json_keeps_the_fractional_form_for_integral_bias() {
        // Edge case: `f64`'s Display prints `0.0` as `0`; the wire format the
        // hand-written literals used carried `0.0`.
        assert_eq!(
            neuron_json("output", "output-0", 0.0, "IDENTITY"),
            r#"{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}"#
        );
    }

    #[test]
    fn synapse_json_emits_the_wire_shape() {
        assert_eq!(
            synapse_json("input-0", "hidden-0", 1.0),
            r#"{"fromUUID":"input-0","toUUID":"hidden-0","weight":1.0}"#
        );
    }

    #[test]
    fn typed_synapse_json_appends_the_aggregate_input_type() {
        assert_eq!(
            typed_synapse_json("input-1", "h-if", 0.8, Some("positive")),
            r#"{"fromUUID":"input-1","toUUID":"h-if","weight":0.8,"type":"positive"}"#
        );
        assert_eq!(
            typed_synapse_json("input-1", "h-if", 0.8, None),
            synapse_json("input-1", "h-if", 0.8),
            "`None` must emit the plain three-field shape"
        );
    }

    #[test]
    fn creature_envelope_parses_and_round_trips_its_parts() {
        let json = creature_envelope(
            1,
            1,
            &[neuron_json("output", "output-0", 0.0, "IDENTITY")],
            &[synapse_json("input-0", "output-0", 1.0)],
        );
        let creature = parse_creature_json(&json).expect("envelope parses");
        assert_eq!(creature.input, 1);
        assert_eq!(creature.output, 1);
        assert!(creature.forward_only, "the envelope is always forward-only");
        assert_eq!(creature.neurons.len(), 1);
        assert_eq!(creature.synapses.len(), 1);
        assert_eq!(creature.synapses[0].from_uuid, "input-0");
    }

    #[test]
    fn a_non_finite_weight_fails_loudly_at_parse() {
        // Error path: JSON has no `NaN`/`Infinity` literal. Rather than coerce
        // a caller's overflowed weight to something plausible, the emitter
        // renders it verbatim so the corrupt fixture is rejected at parse
        // instead of scoring silently against a substituted value.
        let json = creature_envelope(
            1,
            1,
            &[neuron_json("output", "output-0", 0.0, "IDENTITY")],
            &[synapse_json("input-0", "output-0", f64::NAN)],
        );
        assert!(
            parse_creature_json(&json).is_err(),
            "a NaN weight must not parse as a creature"
        );
    }

    #[test]
    fn dense_mlp_creature_json_has_the_expected_topology() {
        let json = dense_mlp_creature_json(8, 2, 4, "TANH");
        let creature = parse_creature_json(&json).expect("dense MLP parses");
        assert_eq!(creature.input, 8);
        assert_eq!(creature.output, 2);
        assert!(creature.forward_only);
        assert_eq!(creature.neurons.len(), 4 + 2, "hidden + output");
        assert_eq!(creature.synapses.len(), 8 * 4 + 4 * 2, "fully connected");
        for n in creature
            .neurons
            .iter()
            .filter(|n| n.neuron_type == "hidden")
        {
            assert_eq!(n.squash.as_deref(), Some("TANH"));
        }
    }

    #[test]
    fn dense_mlp_creature_json_honours_the_hidden_squash() {
        let json = dense_mlp_creature_json(2, 1, 3, "ReLU");
        let creature = parse_creature_json(&json).expect("parses");
        let hidden: Vec<_> = creature
            .neurons
            .iter()
            .filter(|n| n.neuron_type == "hidden")
            .collect();
        assert_eq!(hidden.len(), 3);
        for n in hidden {
            assert_eq!(n.squash.as_deref(), Some("ReLU"));
        }
    }

    #[test]
    fn dense_mlp_creature_json_with_no_hidden_layer_still_parses() {
        // Edge case: `hidden == 0` degenerates to an unwired output-only
        // creature. It must still emit a parseable envelope.
        let json = dense_mlp_creature_json(2, 1, 0, "TANH");
        let creature = parse_creature_json(&json).expect("parses");
        assert_eq!(creature.neurons.len(), 1, "outputs only");
        assert!(creature.synapses.is_empty());
    }
}
