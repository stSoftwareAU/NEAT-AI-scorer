//! neat-core#569 — a creature carrying `memetic.weights` in either valid wire
//! form flows through the scorer unchanged.
//!
//! neat-core 0.10.0 retyped the public `MemeticExport::weights` field from
//! `BTreeMap<String, Vec<MemeticWeightExport>>` to the `MemeticWeights` enum so
//! that both NEAT-AI wire forms parse — the UUID-keyed row array written by
//! `MemeticWireExport.ts` (the form that cost the fleet a Backprop stage) and
//! the id-keyed map. `rust_scorer` never reads memetic weights,
//! but every creature it scores is parsed by neat-core, so this is the
//! behaviour the acknowledged breaking bump has to preserve: both forms parse,
//! neither changes any score component, and an invalid form still fails loud.
//!
//! These assertions cannot hold against neat-core < 0.10.0: the row form is
//! rejected there with `invalid type: sequence, expected a map`.

use neat_core::creature::{compile_creature, parse_creature_json};
use rust_scorer::fixture_json::{creature_envelope, neuron_json, synapse_json};
use rust_scorer::scoring::compute_score_components;

/// The two-input, one-hidden, one-output creature every case below scores,
/// optionally carrying a `memetic` block spliced in before the closing brace.
fn creature_json(memetic: Option<&str>) -> String {
    let neurons = vec![
        neuron_json("hidden", "hidden-0", 0.5, "TANH"),
        neuron_json("output", "output-0", -0.3, "IDENTITY"),
    ];
    let synapses = vec![
        synapse_json("input-0", "hidden-0", 1.5),
        synapse_json("input-1", "hidden-0", -0.8),
        synapse_json("hidden-0", "output-0", 2.0),
    ];
    let envelope = creature_envelope(2, 1, &neurons, &synapses);
    match memetic {
        None => envelope,
        Some(block) => {
            let body = envelope
                .strip_suffix('}')
                .expect("the envelope is a JSON object");
            format!(r#"{body},"memetic":{block}}}"#)
        }
    }
}

/// The UUID-keyed row array — `MemeticWeightWireRow[]`, the form NEAT-AI
/// writes for any JSON that leaves the process.
const ROW_FORM: &str = r#"{"biases":{"output-0":0.25},"weights":[
    {"fromUUID":"input-0","toUUID":"hidden-0","weight":0.125},
    {"fromUUID":"hidden-0","toUUID":"output-0","weight":-0.5}
]}"#;

/// The id-keyed map — `MemeticWeightsInterface`.
const MAP_FORM: &str = r#"{"biases":{"-1":0.25},"weights":{
    "1":[{"toId":-1,"weight":-0.5}]
}}"#;

#[test]
fn row_form_memetic_creature_parses_compiles_and_scores() {
    let creature = parse_creature_json(&creature_json(Some(ROW_FORM)))
        .expect("the UUID-keyed row form must parse");

    let memetic = creature
        .memetic
        .as_ref()
        .expect("memetic survives the parse");
    let rows = memetic
        .weights
        .rows()
        .expect("the row form parses as MemeticWeights::Rows");
    assert_eq!(rows.len(), 2);

    compile_creature(&creature).expect("a memetic creature still compiles");
    let components = compute_score_components(&creature).expect("and still scores");
    assert_eq!(components.hidden_neuron_count, 1);
    assert_eq!(components.synapse_count, 3);
}

#[test]
fn map_form_memetic_creature_parses_compiles_and_scores() {
    let creature = parse_creature_json(&creature_json(Some(MAP_FORM)))
        .expect("the id-keyed map form must still parse");

    let memetic = creature
        .memetic
        .as_ref()
        .expect("memetic survives the parse");
    let by_id = memetic
        .weights
        .by_id()
        .expect("the map form parses as MemeticWeights::ById");
    assert_eq!(by_id.len(), 1);

    compile_creature(&creature).expect("a memetic creature still compiles");
    compute_score_components(&creature).expect("and still scores");
}

#[test]
fn memetic_weights_never_move_a_score_component() {
    let plain = compute_score_components(
        &parse_creature_json(&creature_json(None)).expect("the plain creature parses"),
    )
    .expect("the plain creature scores");

    for (label, form) in [("row", ROW_FORM), ("map", MAP_FORM)] {
        let scored = compute_score_components(
            &parse_creature_json(&creature_json(Some(form))).expect("the memetic creature parses"),
        )
        .expect("the memetic creature scores");
        assert_eq!(
            scored.hidden_neuron_count, plain.hidden_neuron_count,
            "{label} form moved hidden_neuron_count"
        );
        assert_eq!(
            scored.synapse_count, plain.synapse_count,
            "{label} form moved synapse_count"
        );
        assert!(
            (scored.max_weight_bias - plain.max_weight_bias).abs() < f64::EPSILON,
            "{label} form moved max_weight_bias"
        );
        assert!(
            (scored.avg_weight_bias - plain.avg_weight_bias).abs() < f64::EPSILON,
            "{label} form moved avg_weight_bias"
        );
        assert!(
            (scored.squash_complexity_penalty - plain.squash_complexity_penalty).abs()
                < f64::EPSILON,
            "{label} form moved squash_complexity_penalty"
        );
    }
}

#[test]
fn a_memetic_weights_value_that_is_neither_form_fails_loud() {
    let err = parse_creature_json(&creature_json(Some(r#"{"weights":7}"#)))
        .expect_err("a scalar `weights` is neither valid form and must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("weights"),
        "the failure must name the offending field, was: {message}"
    );
}
