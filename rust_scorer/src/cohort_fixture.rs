//! Deterministic near-clone cohorts for the cohort-scoring parity tests and
//! bench (Issue #683).
//!
//! A [`CreatureSpec`] is an in-memory creature that can be edited the way the
//! optimisers edit one — graft an `IF` correction onto an anchor, change one
//! weight, remove one neuron — and then rendered through
//! [`crate::fixture_json`] (Issue #513: no hand-encoded wire format).
//!
//! [`layered_creature`] builds a sparse layered network from a seed; with
//! [`LayeredShape::production_scale`] it has the size of the production
//! creature named in Issue #683 (7 804 non-input neurons, 65 687 synapses,
//! 2 511 inputs). It is synthetic: the production creature is private and is never
//! fetched by this repository.

use crate::fixture_json::{
    constant_neuron_json, creature_envelope, neuron_json, typed_synapse_json,
};

/// UUID of the constant `1.0` neuron every layered creature carries; `IF`
/// grafts read their threshold through it.
pub const CONST_ONE: &str = "const-one";
/// Constant feeding every graft's positive branch.
pub const CONST_POSITIVE: &str = "const-positive";
/// Constant feeding every graft's negative branch.
pub const CONST_NEGATIVE: &str = "const-negative";

/// Small deterministic PRNG (SplitMix64) so fixtures are reproducible from a
/// seed without a dependency.
#[derive(Debug, Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    /// Seeded generator.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Next raw 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1_u64 << 53) as f64
    }

    /// Uniform in `[lo, hi)`.
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    /// Uniform index in `0..n` (`n > 0`).
    pub fn index(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// One non-input neuron.
#[derive(Debug, Clone, PartialEq)]
pub struct NeuronSpec {
    /// Neuron UUID.
    pub uuid: String,
    /// `"hidden"`, `"output"` or `"constant"`.
    pub kind: &'static str,
    /// Bias (the value, for a constant).
    pub bias: f64,
    /// Squash name (ignored for a constant).
    pub squash: String,
}

/// One synapse.
#[derive(Debug, Clone, PartialEq)]
pub struct SynapseSpec {
    /// Source UUID.
    pub from: String,
    /// Target UUID.
    pub to: String,
    /// Weight.
    pub weight: f64,
    /// Aggregate role (`"condition"`, `"positive"`, `"negative"`), if any.
    pub role: Option<&'static str>,
}

/// An editable creature.
#[derive(Debug, Clone, PartialEq)]
pub struct CreatureSpec {
    /// Input count.
    pub inputs: usize,
    /// Output count.
    pub outputs: usize,
    /// Non-input neurons in evaluation order.
    pub neurons: Vec<NeuronSpec>,
    /// Synapses.
    pub synapses: Vec<SynapseSpec>,
}

impl CreatureSpec {
    /// Render the creature JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        let neurons: Vec<String> = self
            .neurons
            .iter()
            .map(|n| {
                if n.kind == "constant" {
                    constant_neuron_json(&n.uuid, n.bias)
                } else {
                    neuron_json(n.kind, &n.uuid, n.bias, &n.squash)
                }
            })
            .collect();
        let synapses: Vec<String> = self
            .synapses
            .iter()
            .map(|s| typed_synapse_json(&s.from, &s.to, s.weight, s.role))
            .collect();
        creature_envelope(self.inputs, self.outputs, &neurons, &synapses)
    }

    /// Non-input neuron count.
    #[must_use]
    pub fn neuron_count(&self) -> usize {
        self.neurons.len()
    }

    /// Synapse count.
    #[must_use]
    pub fn synapse_count(&self) -> usize {
        self.synapses.len()
    }

    fn position(&self, uuid: &str) -> usize {
        self.neurons
            .iter()
            .position(|n| n.uuid == uuid)
            .unwrap_or_else(|| panic!("no neuron {uuid}"))
    }

    /// Graft `count` depth-1 `IF` corrections onto `anchor`: each reads one
    /// input column against a threshold and feeds a constant per branch into
    /// the anchor. The `IF` neurons are inserted just before the anchor so
    /// evaluation order stays topological. `tag` keeps UUIDs unique per graft.
    #[must_use]
    pub fn with_if_graft(
        &self,
        anchor: &str,
        count: usize,
        tag: &str,
        rng: &mut SplitMix64,
    ) -> Self {
        let mut out = self.clone();
        let at = out.position(anchor);
        for g in 0..count {
            let uuid = format!("graft-{tag}-{g}");
            out.neurons.insert(
                at + g,
                NeuronSpec {
                    uuid: uuid.clone(),
                    kind: "hidden",
                    bias: 0.0,
                    squash: "IF".to_string(),
                },
            );
            let feature = rng.index(self.inputs);
            let threshold = rng.range(-0.5, 0.5);
            let syn = |from: String, weight: f64, role| SynapseSpec {
                from,
                to: uuid.clone(),
                weight,
                role: Some(role),
            };
            out.synapses
                .push(syn(format!("input-{feature}"), 1.0, "condition"));
            out.synapses
                .push(syn(CONST_ONE.to_string(), -threshold, "condition"));
            out.synapses.push(syn(
                CONST_POSITIVE.to_string(),
                rng.range(-0.3, 0.3),
                "positive",
            ));
            out.synapses.push(syn(
                CONST_NEGATIVE.to_string(),
                rng.range(-0.3, 0.3),
                "negative",
            ));
            out.synapses.push(SynapseSpec {
                from: uuid,
                to: anchor.to_string(),
                weight: rng.range(0.05, 0.5),
                role: None,
            });
        }
        out
    }

    /// Change the weight of synapse `index` by `delta`.
    #[must_use]
    pub fn with_weight_change(&self, index: usize, delta: f64) -> Self {
        let mut out = self.clone();
        out.synapses[index].weight += delta;
        out
    }

    /// Remove hidden neuron `uuid` and every synapse touching it.
    #[must_use]
    pub fn without_neuron(&self, uuid: &str) -> Self {
        let mut out = self.clone();
        out.neurons.retain(|n| n.uuid != uuid);
        out.synapses.retain(|s| s.from != uuid && s.to != uuid);
        out
    }
}

/// Shape of a [`layered_creature`].
#[derive(Debug, Clone)]
pub struct LayeredShape {
    /// Input count.
    pub inputs: usize,
    /// Hidden layers as `(width, fan_in)`; layer 0 reads inputs, layer `k`
    /// reads layer `k - 1`.
    pub layers: Vec<(usize, usize)>,
    /// Output count; each output reads every neuron of the last layer.
    pub outputs: usize,
    /// Squash names cycled across hidden neurons.
    pub squashes: Vec<&'static str>,
    /// Extra fan-in of the last hidden layer's first neuron (the graft
    /// anchor), drawn from the layer below; production anchors have fan-in in
    /// the thousands. The anchor's total fan-in (`fan_in + anchor_fan_in`) must
    /// not exceed the layer below's width.
    pub anchor_fan_in: usize,
    /// PRNG seed.
    pub seed: u64,
}

impl LayeredShape {
    /// Production-scale shape (Issue #683): 2 511 inputs, 7 804 non-input
    /// neurons (10 315 nodes in all), 65 687 synapses, and a graft anchor with
    /// fan-in 1 500 — every neuron of the layer below.
    #[must_use]
    pub fn production_scale(seed: u64) -> Self {
        Self {
            inputs: 2511,
            layers: vec![(6000, 7), (1500, 12), (300, 13)],
            outputs: 1,
            squashes: vec!["TANH", "LOGISTIC", "RELU", "IDENTITY", "GELU", "TANH"],
            anchor_fan_in: 1487,
            seed,
        }
    }

    /// Small shape for parity tests, with `MINIMUM`/`MAXIMUM` aggregates and
    /// two outputs.
    #[must_use]
    pub fn small(seed: u64) -> Self {
        Self {
            inputs: 12,
            layers: vec![(16, 4), (8, 5), (4, 6)],
            outputs: 2,
            squashes: vec![
                "TANH", "LOGISTIC", "RELU", "IDENTITY", "GELU", "MINIMUM", "SINE", "MAXIMUM",
            ],
            anchor_fan_in: 2,
            seed,
        }
    }
}

/// UUID of hidden neuron `i` of layer `layer`.
#[must_use]
pub fn hidden_uuid(layer: usize, i: usize) -> String {
    format!("h{layer}-{i}")
}

/// UUID of the graft anchor: the first neuron of the last hidden layer.
#[must_use]
pub fn anchor_uuid(shape: &LayeredShape) -> String {
    hidden_uuid(shape.layers.len() - 1, 0)
}

/// `count` distinct indices in `0..pool`. Refuses a `count` above `pool`
/// rather than silently building a smaller fan-in than the shape declares.
fn distinct_sources(rng: &mut SplitMix64, pool: usize, count: usize) -> Vec<usize> {
    assert!(
        count <= pool,
        "fan-in {count} exceeds the {pool}-neuron source pool"
    );
    let mut picked: Vec<usize> = Vec::with_capacity(count);
    while picked.len() < count {
        let i = rng.index(pool);
        if !picked.contains(&i) {
            picked.push(i);
        }
    }
    picked
}

/// Build a sparse layered creature from `shape`.
#[must_use]
pub fn layered_creature(shape: &LayeredShape) -> CreatureSpec {
    let mut rng = SplitMix64::new(shape.seed);
    let mut neurons = vec![
        NeuronSpec {
            uuid: CONST_ONE.to_string(),
            kind: "constant",
            bias: 1.0,
            squash: String::new(),
        },
        NeuronSpec {
            uuid: CONST_POSITIVE.to_string(),
            kind: "constant",
            bias: 1.0,
            squash: String::new(),
        },
        NeuronSpec {
            uuid: CONST_NEGATIVE.to_string(),
            kind: "constant",
            bias: 1.0,
            squash: String::new(),
        },
    ];
    let mut synapses = Vec::new();
    let mut squash_at = 0_usize;
    let last = shape.layers.len() - 1;
    for (layer, &(width, fan_in)) in shape.layers.iter().enumerate() {
        let (pool, pool_uuid): (usize, Box<dyn Fn(usize) -> String>) = if layer == 0 {
            (shape.inputs, Box::new(|i| format!("input-{i}")))
        } else {
            let below = layer - 1;
            (
                shape.layers[below].0,
                Box::new(move |i| hidden_uuid(below, i)),
            )
        };
        for i in 0..width {
            let uuid = hidden_uuid(layer, i);
            let squash = shape.squashes[squash_at % shape.squashes.len()];
            squash_at += 1;
            neurons.push(NeuronSpec {
                uuid: uuid.clone(),
                kind: "hidden",
                bias: rng.range(-0.1, 0.1),
                squash: squash.to_string(),
            });
            let fan = if layer == last && i == 0 {
                fan_in + shape.anchor_fan_in
            } else {
                fan_in
            };
            let scale = 1.5 / (fan as f64).sqrt();
            for src in distinct_sources(&mut rng, pool, fan) {
                synapses.push(SynapseSpec {
                    from: pool_uuid(src),
                    to: uuid.clone(),
                    weight: rng.range(-scale, scale),
                    role: None,
                });
            }
        }
    }
    let last_width = shape.layers[last].0;
    for o in 0..shape.outputs {
        let uuid = format!("output-{o}");
        neurons.push(NeuronSpec {
            uuid: uuid.clone(),
            kind: "output",
            bias: 0.0,
            squash: "IDENTITY".to_string(),
        });
        for i in 0..last_width {
            synapses.push(SynapseSpec {
                from: hidden_uuid(last, i),
                to: uuid.clone(),
                weight: rng.range(-0.3, 0.3),
                role: None,
            });
        }
    }
    CreatureSpec {
        inputs: shape.inputs,
        outputs: shape.outputs,
        neurons,
        synapses,
    }
}

/// The incumbent plus `n - 1` candidates, each grafted with 1–4 `IF` neurons
/// at the shape's anchor.
#[must_use]
pub fn graft_cohort(
    base: &CreatureSpec,
    shape: &LayeredShape,
    n: usize,
    seed: u64,
) -> Vec<CreatureSpec> {
    let mut rng = SplitMix64::new(seed);
    let anchor = anchor_uuid(shape);
    let mut out = vec![base.clone()];
    for c in 1..n {
        let count = 1 + rng.index(4);
        out.push(base.with_if_graft(&anchor, count, &c.to_string(), &mut rng));
    }
    out
}

/// `n` candidates, each with one weight changed somewhere in the network.
#[must_use]
pub fn weight_change_cohort(base: &CreatureSpec, n: usize, seed: u64) -> Vec<CreatureSpec> {
    let mut rng = SplitMix64::new(seed);
    (0..n)
        .map(|_| {
            let i = rng.index(base.synapse_count());
            base.with_weight_change(i, rng.range(-0.05, 0.05))
        })
        .collect()
}

/// `n` candidates, each with one hidden neuron (not the anchor or the last
/// layer) removed.
#[must_use]
pub fn removal_cohort(
    base: &CreatureSpec,
    shape: &LayeredShape,
    n: usize,
    seed: u64,
) -> Vec<CreatureSpec> {
    let mut rng = SplitMix64::new(seed);
    let removable = shape.layers.len().saturating_sub(1).max(1);
    (0..n)
        .map(|_| {
            let layer = rng.index(removable);
            let i = rng.index(shape.layers[layer].0);
            base.without_neuron(&hidden_uuid(layer, i))
        })
        .collect()
}

/// `n` packed `[inputs…, targets…]` records with inputs in `[-1, 1)` and a
/// smooth target per output.
#[must_use]
pub fn corpus(inputs: usize, outputs: usize, n: usize, seed: u64) -> Vec<f32> {
    let mut rng = SplitMix64::new(seed);
    let mut out = Vec::with_capacity(n * (inputs + outputs));
    for _ in 0..n {
        let start = out.len();
        for _ in 0..inputs {
            out.push(rng.range(-1.0, 1.0) as f32);
        }
        let s: f32 = out[start..].iter().take(8).sum();
        for o in 0..outputs {
            out.push(((o as f32 + 1.0) * s).sin() * 0.5);
        }
    }
    out
}

/// Little-endian bytes of `records`, the `.bin` corpus format.
#[must_use]
pub fn to_le_bytes(records: &[f32]) -> Vec<u8> {
    records.iter().flat_map(|v| v.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fan_in(spec: &CreatureSpec, uuid: &str) -> usize {
        spec.synapses.iter().filter(|s| s.to == uuid).count()
    }

    #[test]
    fn production_scale_shape_is_what_the_documents_say() {
        let shape = LayeredShape::production_scale(683);
        let base = layered_creature(&shape);
        assert_eq!(base.inputs, 2511);
        assert_eq!(base.neuron_count(), 7804);
        assert_eq!(base.inputs + base.neuron_count(), 10_315);
        assert_eq!(base.synapse_count(), 65_687);
        assert_eq!(fan_in(&base, &anchor_uuid(&shape)), 1500);
    }

    #[test]
    fn small_shape_anchor_has_its_declared_fan_in() {
        let shape = LayeredShape::small(1);
        let base = layered_creature(&shape);
        let (_, fan) = shape.layers[shape.layers.len() - 1];
        assert_eq!(
            fan_in(&base, &anchor_uuid(&shape)),
            fan + shape.anchor_fan_in
        );
    }

    #[test]
    #[should_panic(expected = "exceeds the 8-neuron source pool")]
    fn an_impossible_fan_in_is_refused() {
        let mut shape = LayeredShape::small(1);
        shape.anchor_fan_in = 3;
        let _ = layered_creature(&shape);
    }
}
