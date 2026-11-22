//! Cohort (delta) scoring — Issue #683.
//!
//! Every optimiser in the family asks the scorer about creatures that are
//! almost identical to one incumbent: a graft cohort is the incumbent plus one
//! small tree each, a weight-change cohort differs in one synapse, a removal
//! cohort is the incumbent minus one neuron. Directory mode scores each as if
//! it were unrelated, so `N` candidates cost `N` full forward passes per record.
//!
//! Cohort mode builds **one union network** for the whole directory, in which
//! every computation the candidates share appears exactly once, and activates
//! that union once per record. A candidate then costs only the neurons it
//! changed plus everything downstream of them — its *dirty cone* — not the
//! size of the creature.
//!
//! ## Structural hashing
//!
//! Two neurons are the same computation when they are both constants with the
//! same bias, or when squash, bias and the full ordered inbound list
//! `(source, weight, role)` match and every source is, recursively, the same
//! computation. Compiled networks are in topological order, so one pass that
//! maps each neuron's sources to their union ids *before* hashing the neuron
//! decides this exactly ([`CohortPlan::build`]). Floats are compared by bit
//! pattern, so `0.0` and `-0.0` are different computations and no tolerance is
//! involved. UUIDs play no part, and no caller-supplied diff is consulted: the
//! scorer discovers what is shared.
//!
//! Inbound **order** is part of the key because it is part of the arithmetic:
//! an `f32` weighted sum is evaluated in synapse order, so two neurons with the
//! same synapses in a different order are kept apart rather than risk a
//! rounding difference.
//!
//! ## Parity contract
//!
//! The union is evaluated with the same public `neat-core` kernels, in the same
//! per-record arithmetic, as the fused MSE scan scores a creature with: records
//! that land in a 4-or-more SIMD group of a worker range go through the
//! record-interleaved gather (`bias` seeded, one FMA per synapse in order, the
//! vectorised squash, the range clamp), the `n % 4` tail through the
//! single-record kernels, and per-record squared errors are folded into an
//! `f64` in record order. The chunk is partitioned across workers exactly as
//! directory mode partitions it for a **one-creature** directory, so a
//! creature's cohort result is **bit-identical** to scoring that creature alone
//! in a directory. Scored inside a larger directory, directory mode itself
//! regroups records per worker, which moves a score by `f32` rounding
//! (≈ 1e-9 relative, the Issue #537 regrouping tolerance); cohort mode is held
//! to that tolerance against such a run, and candidate ordering is exact. See
//! the README [Cohort scoring](../../README.md#cohort-scoring-issue-683)
//! section.
//!
//! ## What the mode cannot host
//!
//! [`CohortPlan::build`] refuses — and the CLI falls back to ordinary directory
//! mode with one `[cohort] fallback to directory mode: …` stderr line — when:
//!
//! * the cost is not `MSE`/`RMSE` (every other cost groups records into SIMD
//!   tiers by its own rule, which this evaluator does not mirror);
//! * any creature is recurrent (`forwardOnly: false`);
//! * any neuron uses `HYPOT`, `HYPOTv2` or `MEAN`, whose exact single-record
//!   kernels need a per-record activation slice the interleaved union does not
//!   keep;
//! * the union would exceed [`MAX_NODE_COUNT`] nodes (the `u16` synapse source
//!   index `neat-core` kernels read).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;

use neat_core::network::MAX_NODE_COUNT;
use neat_core::range::{apply_get_range, apply_limit_range, apply_limit_range_bounds};
use neat_core::simd::{weighted_sum_interleaved_unchecked, weighted_sum_simd_unchecked};
use neat_core::squash_simd::squash_xn;
use neat_core::{CompiledNetwork, NeuronData, SquashType, SynapseData, SynapseType, apply_squash};

use crate::cost::CostKind;

/// Record tile width of the interleaved union pass. Per-lane arithmetic is
/// identical at every tile width (`neat-core` `MSE_TILE_LANES` contract), so
/// this is a pure memory-traffic choice; 32 matches the fused MSE scan.
pub const COHORT_TILE_LANES: usize = 32;

/// Prefix of every stderr line the cohort path prints, so a caller can grep
/// for it (Issue #683).
pub const COHORT_LOG_PREFIX: &str = "[cohort]";

/// Why a cohort cannot be hosted by the union evaluator. Each is a routing
/// decision, not a scoring failure: the caller scores the directory on the
/// ordinary path instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CohortRejection {
    /// The requested cost is not `MSE` or `RMSE`.
    UnsupportedCost(&'static str),
    /// A creature is recurrent (`forwardOnly: false`).
    Recurrent(String),
    /// A creature uses a squash the interleaved union cannot evaluate exactly.
    UnsupportedSquash {
        /// Creature file stem.
        creature: String,
        /// Squash type code.
        squash_type: u8,
    },
    /// A compiled neuron reads a source at or after itself.
    NotTopological(String),
    /// The union network would exceed [`MAX_NODE_COUNT`] nodes.
    TooManyNodes(usize),
}

impl fmt::Display for CohortRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedCost(name) => {
                write!(f, "cost {name} is not hosted (MSE and RMSE only)")
            }
            Self::Recurrent(c) => write!(f, "creature '{c}' is recurrent (forwardOnly=false)"),
            Self::UnsupportedSquash {
                creature,
                squash_type,
            } => write!(
                f,
                "creature '{creature}' uses squash {:?} (HYPOT, HYPOTv2 and MEAN are not hosted)",
                SquashType::from(*squash_type)
            ),
            Self::NotTopological(c) => {
                write!(f, "creature '{c}' is not in forward topological order")
            }
            Self::TooManyNodes(n) => {
                write!(
                    f,
                    "union network would hold {n} nodes (limit {MAX_NODE_COUNT})"
                )
            }
        }
    }
}

/// What the union saved, reported once per run on stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CohortStats {
    /// Number of creatures in the cohort.
    pub creatures: usize,
    /// Non-input neurons activated per record by cohort mode (the union).
    pub union_neurons: usize,
    /// Non-input neurons directory mode activates per record (sum over creatures).
    pub standalone_neurons: usize,
    /// Synapses read per record by cohort mode.
    pub union_synapses: usize,
    /// Synapses directory mode reads per record.
    pub standalone_synapses: usize,
}

impl fmt::Display for CohortStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ratio = |a: usize, b: usize| if b == 0 { 0.0 } else { a as f64 / b as f64 };
        write!(
            f,
            "{COHORT_LOG_PREFIX} hosted {} creatures: {} neuron activations per record (directory mode {}, {:.4}x), {} synapses per record (directory mode {}, {:.4}x)",
            self.creatures,
            self.union_neurons,
            self.standalone_neurons,
            ratio(self.union_neurons, self.standalone_neurons),
            self.union_synapses,
            self.standalone_synapses,
            ratio(self.union_synapses, self.standalone_synapses),
        )
    }
}

/// Structural identity of one union node.
#[derive(Debug, PartialEq, Eq, Hash)]
enum NodeKey {
    /// A constant neuron emits its (clamped) bias; inbound synapses are inert.
    Constant { bias: u32 },
    /// A computed neuron: squash, bias and the ordered inbound list
    /// `(union source id, weight bits, synapse role)`.
    Computed {
        squash: u8,
        bias: u32,
        inbound: Vec<(u32, u32, u8)>,
    },
}

/// One creature of the cohort, as the planner sees it.
pub struct CohortMember<'a> {
    /// File stem, for diagnostics.
    pub key: &'a str,
    /// The creature compiled by `neat-core`.
    pub network: &'a CompiledNetwork,
    /// The creature's `forwardOnly` flag.
    pub forward_only: bool,
}

/// The union network plus, per creature, the union ids of its outputs.
pub struct CohortPlan {
    union: CompiledNetwork,
    num_inputs: usize,
    num_outputs: usize,
    /// `outputs[c][j]` — union node id of creature `c`'s output `j`.
    outputs: Vec<Vec<u32>>,
    stats: CohortStats,
}

fn squash_is_hosted(squash_type: u8) -> bool {
    !matches!(
        SquashType::from(squash_type),
        SquashType::Hypotenuse | SquashType::HypotenuseV2 | SquashType::Mean
    )
}

impl CohortPlan {
    /// Hash-cons every member into one union network.
    ///
    /// All members must share `num_inputs`/`num_outputs` (directory loading
    /// already enforces this). Returns the reason the cohort cannot be hosted
    /// instead of a plan when any precondition in the module docs fails.
    pub fn build(
        members: &[CohortMember<'_>],
        num_inputs: usize,
        num_outputs: usize,
        cost: CostKind,
    ) -> Result<Self, CohortRejection> {
        if !matches!(cost, CostKind::Mse | CostKind::Rmse) {
            return Err(CohortRejection::UnsupportedCost(cost.as_str()));
        }
        let mut index: HashMap<NodeKey, u32> = HashMap::new();
        let mut neurons: Vec<NeuronData> = Vec::new();
        let mut synapses: Vec<SynapseData> = Vec::new();
        let mut outputs = Vec::with_capacity(members.len());
        let mut standalone_neurons = 0_usize;
        let mut standalone_synapses = 0_usize;

        for m in members {
            if !m.forward_only {
                return Err(CohortRejection::Recurrent(m.key.to_string()));
            }
            let net = m.network;
            let c_neurons = net.neurons();
            let c_synapses = net.synapses();
            standalone_neurons += c_neurons.len();
            standalone_synapses += c_neurons
                .iter()
                .filter(|n| !n.is_constant)
                .map(|n| n.num_synapses as usize)
                .sum::<usize>();
            // Creature node index -> union node id. Inputs are shared as-is.
            let mut map: Vec<u32> = (0..num_inputs as u32).collect();
            map.reserve(c_neurons.len());
            for (j, n) in c_neurons.iter().enumerate() {
                let own = num_inputs + j;
                let key = if n.is_constant {
                    NodeKey::Constant {
                        bias: n.bias.to_bits(),
                    }
                } else {
                    if !squash_is_hosted(n.squash_type) {
                        return Err(CohortRejection::UnsupportedSquash {
                            creature: m.key.to_string(),
                            squash_type: n.squash_type,
                        });
                    }
                    let start = n.start_synapse as usize;
                    let end = start + n.num_synapses as usize;
                    let mut inbound = Vec::with_capacity(end - start);
                    for s in &c_synapses[start..end] {
                        let from = s.from_index as usize;
                        if from >= own {
                            return Err(CohortRejection::NotTopological(m.key.to_string()));
                        }
                        inbound.push((map[from], s.weight.to_bits(), s.synapse_type));
                    }
                    NodeKey::Computed {
                        squash: n.squash_type,
                        bias: n.bias.to_bits(),
                        inbound,
                    }
                };
                let id = match index.entry(key) {
                    Entry::Occupied(e) => *e.get(),
                    Entry::Vacant(e) => {
                        let id = num_inputs + neurons.len();
                        if id >= MAX_NODE_COUNT {
                            return Err(CohortRejection::TooManyNodes(id + 1));
                        }
                        let start_synapse = synapses.len() as u32;
                        let num_synapses = match e.key() {
                            NodeKey::Constant { .. } => 0_u16,
                            NodeKey::Computed { inbound, .. } => {
                                for &(from, weight, role) in inbound {
                                    synapses.push(SynapseData {
                                        weight: f32::from_bits(weight),
                                        from_index: from as u16,
                                        synapse_type: role,
                                    });
                                }
                                n.num_synapses
                            }
                        };
                        neurons.push(NeuronData {
                            bias: n.bias,
                            start_synapse,
                            num_synapses,
                            squash_type: n.squash_type,
                            is_constant: n.is_constant,
                        });
                        e.insert(id as u32);
                        id as u32
                    }
                };
                map.push(id);
            }
            let total = map.len();
            outputs.push(map[total - num_outputs..].to_vec());
        }

        let union_neurons = neurons.len();
        let union_synapses = neurons
            .iter()
            .filter(|n| !n.is_constant)
            .map(|n| n.num_synapses as usize)
            .sum();
        let union = CompiledNetwork::from_parts(num_inputs, neurons, synapses)
            .map_err(|_| CohortRejection::TooManyNodes(num_inputs + union_neurons))?;
        Ok(Self {
            union,
            num_inputs,
            num_outputs,
            outputs,
            stats: CohortStats {
                creatures: members.len(),
                union_neurons,
                standalone_neurons,
                union_synapses,
                standalone_synapses,
            },
        })
    }

    /// What the union saves against directory mode.
    #[must_use]
    pub fn stats(&self) -> CohortStats {
        self.stats
    }

    /// Number of creatures in the cohort.
    #[must_use]
    pub fn len(&self) -> usize {
        self.outputs.len()
    }

    /// True for an empty cohort.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }

    /// Allocate one worker's reusable scratch.
    #[must_use]
    pub fn scratch(&self) -> CohortScratch {
        let n = self.union.num_neurons();
        CohortScratch {
            inter: vec![0.0; n * COHORT_TILE_LANES],
            act: vec![0.0; n],
        }
    }

    /// Add each creature's summed per-record MSE over `records` (packed
    /// `[inputs…, targets…]`) into `sums[c]`, in record order — the cohort
    /// counterpart of `neat_core::loss::mse_sum_batch_packed` for one worker
    /// range. `sums` must hold one entry per creature.
    pub fn mse_sum_into(&self, scratch: &mut CohortScratch, records: &[f32], sums: &mut [f64]) {
        assert_eq!(sums.len(), self.len(), "one sum per cohort creature");
        let values_per_record = self.num_inputs + self.num_outputs;
        let n = records.len() / values_per_record;
        if n == 0 {
            return;
        }
        let inv_outputs = 1.0 / (self.num_outputs as f64);
        // `neat-core` scores the first `n - n % 4` records of a range in SIMD
        // groups (32/8-record interleaved tiles, then one 4-record group) and
        // the rest one record at a time; a range shorter than 4 is all scalar.
        let grouped = if n >= 4 { n - n % 4 } else { 0 };
        let mut base = 0;
        while base < grouped {
            let lanes = (grouped - base).min(COHORT_TILE_LANES);
            self.tile_forward(scratch, records, values_per_record, base, lanes);
            for l in 0..lanes {
                let target = (base + l) * values_per_record + self.num_inputs;
                for (c, outs) in self.outputs.iter().enumerate() {
                    let mut sq_sum = 0.0_f64;
                    for (j, &o) in outs.iter().enumerate() {
                        let out = scratch.inter[o as usize * COHORT_TILE_LANES + l];
                        let diff = (records[target + j] - out) as f64;
                        sq_sum += diff * diff;
                    }
                    sums[c] += sq_sum * inv_outputs;
                }
            }
            base += lanes;
        }
        for r in grouped..n {
            let rec = &records[r * values_per_record..(r + 1) * values_per_record];
            self.scalar_forward(&mut scratch.act, rec);
            let target = &rec[self.num_inputs..];
            for (c, outs) in self.outputs.iter().enumerate() {
                let mut sq_sum = 0.0_f64;
                for (j, &o) in outs.iter().enumerate() {
                    let diff = (target[j] - scratch.act[o as usize]) as f64;
                    sq_sum += diff * diff;
                }
                sums[c] += sq_sum * inv_outputs;
            }
        }
    }

    /// Interleaved forward pass for `lanes <= COHORT_TILE_LANES` records
    /// starting at record `base`. Unused lanes carry zero inputs and are never
    /// read back.
    fn tile_forward(
        &self,
        scratch: &mut CohortScratch,
        records: &[f32],
        values_per_record: usize,
        base: usize,
        lanes: usize,
    ) {
        const R: usize = COHORT_TILE_LANES;
        let num_inputs = self.num_inputs;
        let inter = &mut scratch.inter;
        // Always-on guard for the unchecked gather below (CONTRIBUTING: a
        // precondition that guards `unsafe` is an `assert!`).
        assert_eq!(inter.len(), self.union.num_neurons() * R);
        for i in 0..num_inputs {
            let row = &mut inter[i * R..i * R + R];
            for (l, slot) in row.iter_mut().enumerate() {
                *slot = if l < lanes {
                    records[(base + l) * values_per_record + i]
                } else {
                    0.0
                };
            }
        }
        let synapses = self.union.synapses();
        let hot_weights = self.union.hot_weights();
        let hot_from = self.union.hot_from();
        for (neuron_idx, neuron) in self.union.neurons().iter().enumerate() {
            let out_base = (num_inputs + neuron_idx) * R;
            if neuron.is_constant {
                let v = apply_limit_range(SquashType::Identity, neuron.bias);
                inter[out_base..out_base + R].fill(v);
                continue;
            }
            let squash = SquashType::from(neuron.squash_type);
            let start = neuron.start_synapse as usize;
            let end = start + neuron.num_synapses as usize;
            if squash.is_aggregate() {
                // MINIMUM / MAXIMUM / IF: the exact single-record rule, lane by
                // lane (HYPOT/HYPOTv2/MEAN are refused by the planner).
                for l in 0..R {
                    let v =
                        aggregate_activation(squash, neuron.bias, &synapses[start..end], |from| {
                            inter[from * R + l]
                        });
                    inter[out_base + l] = apply_limit_range(squash, v);
                }
                continue;
            }
            // SAFETY: `from_parts` validated every `hot_from` entry is
            // `< num_neurons` and every neuron's span lies inside the hot
            // arrays; `inter` is `num_neurons * R` long (asserted above).
            let sums = unsafe {
                weighted_sum_interleaved_unchecked::<R>(
                    hot_weights,
                    hot_from,
                    inter,
                    start,
                    end,
                    neuron.bias,
                )
            };
            let squashed = squash_xn(squash, sums).unwrap_or_else(|| {
                let st = neuron.squash_type;
                sums.map(|s| inline_squash(st, squash, s))
            });
            let (low, high) = apply_get_range(squash);
            for (slot, value) in inter[out_base..out_base + R].iter_mut().zip(squashed) {
                *slot = apply_limit_range_bounds(low, high, value);
            }
        }
    }

    /// Single-record forward pass over the union — the scalar-tail rule.
    fn scalar_forward(&self, act: &mut [f32], record: &[f32]) {
        let num_inputs = self.num_inputs;
        assert_eq!(act.len(), self.union.num_neurons());
        act[..num_inputs].copy_from_slice(&record[..num_inputs]);
        let synapses = self.union.synapses();
        for (neuron_idx, neuron) in self.union.neurons().iter().enumerate() {
            let v = if neuron.is_constant {
                apply_limit_range(SquashType::Identity, neuron.bias)
            } else {
                let squash = SquashType::from(neuron.squash_type);
                let start = neuron.start_synapse as usize;
                let end = start + neuron.num_synapses as usize;
                let activation = if squash.is_aggregate() {
                    aggregate_activation(squash, neuron.bias, &synapses[start..end], |from| {
                        act[from]
                    })
                } else {
                    // SAFETY: `from_parts` validated every `from_index` is
                    // `< num_neurons`, and `act` is `num_neurons` long
                    // (asserted above).
                    let sum = unsafe {
                        weighted_sum_simd_unchecked(synapses, act, start, end, neuron.bias)
                    };
                    inline_squash(neuron.squash_type, squash, sum)
                };
                apply_limit_range(squash, activation)
            };
            act[num_inputs + neuron_idx] = v;
        }
    }
}

/// One worker's reusable buffers for [`CohortPlan::mse_sum_into`].
pub struct CohortScratch {
    inter: Vec<f32>,
    act: Vec<f32>,
}

/// `MINIMUM` / `MAXIMUM` / `IF` exactly as `neat-core`'s single-record kernel
/// computes them (`neuron_activation_scalar`), before the range clamp. `IF`
/// takes the positive branch only when the condition sum is strictly `> 0`
/// (the Issue #574 contract).
#[inline]
fn aggregate_activation(
    squash: SquashType,
    bias: f32,
    synapses: &[SynapseData],
    act: impl Fn(usize) -> f32,
) -> f32 {
    match squash {
        SquashType::Minimum => {
            let mut min_val = f32::INFINITY;
            for s in synapses {
                let val = act(s.from_index as usize) * s.weight;
                if val < min_val {
                    min_val = val;
                }
            }
            if min_val == f32::INFINITY {
                bias
            } else {
                min_val + bias
            }
        }
        SquashType::Maximum => {
            let mut max_val = f32::NEG_INFINITY;
            for s in synapses {
                let val = act(s.from_index as usize) * s.weight;
                if val > max_val {
                    max_val = val;
                }
            }
            if max_val == f32::NEG_INFINITY {
                bias
            } else {
                max_val + bias
            }
        }
        SquashType::If => {
            let mut condition_sum = 0.0_f32;
            let mut positive_sum = 0.0_f32;
            let mut negative_sum = 0.0_f32;
            for s in synapses {
                let val = act(s.from_index as usize) * s.weight;
                match SynapseType::from(s.synapse_type) {
                    SynapseType::Condition => condition_sum += val,
                    SynapseType::Negative => negative_sum += val,
                    SynapseType::Positive | SynapseType::Standard => positive_sum += val,
                }
            }
            if condition_sum > 0.0 {
                positive_sum + bias
            } else {
                negative_sum + bias
            }
        }
        // The planner refuses every other aggregate.
        other => unreachable!("aggregate {other:?} is not hosted by the cohort evaluator"),
    }
}

/// `neat-core`'s hot-squash rule (`batch_scoring::inline_squash`, crate-private
/// upstream): four types inline, the rest through `apply_squash`. Mirrored
/// verbatim; the parity tests fail if the two ever diverge.
#[inline]
fn inline_squash(squash_type: u8, squash: SquashType, sum: f32) -> f32 {
    match squash_type {
        0 => sum,
        1 => sum.max(0.0),
        6 => 1.0 / (1.0 + (-sum).exp()),
        7 => sum.tanh(),
        _ => apply_squash(squash, sum),
    }
}

/// Outcome of [`score_from_creature_dir_cohort`].
pub enum CohortRun {
    /// The union evaluator scored the directory.
    Hosted {
        /// The ordinary stem-keyed result map.
        results: std::collections::BTreeMap<String, crate::scoring::ScoreResult>,
        /// What the union saved.
        stats: CohortStats,
    },
    /// The directory cannot be hosted; nothing was read from the corpus and
    /// the caller should score it on the ordinary path.
    Unhostable(CohortRejection),
}

/// Score every creature in `creatures_dir` in one corpus pass, activating the
/// union network once per record (Issue #683).
///
/// Loading, corpus checks, sampling and finalisation are the directory path's
/// own, so the result map is the one directory mode prints. Returns
/// [`CohortRun::Unhostable`] — before any training data is read — when the
/// cohort cannot be hosted.
pub fn score_from_creature_dir_cohort(
    creatures_dir: &std::path::Path,
    data_path: &std::path::Path,
    cost: CostKind,
    sample: &crate::sampling::SampleSpec,
) -> Result<CohortRun, String> {
    use neat_core::creature::compile_creature;
    use neat_core::training_bin_stream::for_each_read_chunk;
    use neat_core::training_data::{TrainingDataConfig, find_bin_files};
    use rayon::prelude::*;
    use std::time::Instant;

    use crate::multi_score::{
        self, DirectoryRunMeta, finalise_directory_results, load_creatures_from_dir,
        partition_packed_record_ranges, worker_split_factor, workers_per_creature_split,
    };

    let started = Instant::now();
    let loaded = load_creatures_from_dir(creatures_dir)?;
    let num_inputs = loaded[0].creature.input;
    let num_outputs = loaded[0].creature.output;

    let compile_started = Instant::now();
    let mut networks = Vec::with_capacity(loaded.len());
    for c in &loaded {
        multi_score::compile_probe::record_compile();
        networks.push(compile_creature(&c.creature).map_err(|e| {
            format!(
                "Failed compiling worker network for creature '{}': {e}",
                c.path.display()
            )
        })?);
    }
    let members: Vec<CohortMember<'_>> = loaded
        .iter()
        .zip(&networks)
        .map(|(c, network)| CohortMember {
            key: &c.key,
            network,
            forward_only: c.creature.forward_only,
        })
        .collect();
    let plan = match CohortPlan::build(&members, num_inputs, num_outputs, cost) {
        Ok(plan) => plan,
        Err(reason) => return Ok(CohortRun::Unhostable(reason)),
    };
    drop(members);
    drop(networks);
    let compile_time_secs = compile_started.elapsed().as_secs_f64();

    if !data_path.is_dir() {
        return Err(format!(
            "Training data path '{}' is not a directory",
            data_path.display()
        ));
    }
    let bin_files = find_bin_files(data_path)
        .map_err(|e| format!("Failed to read training data directory: {e}"))?;
    if bin_files.is_empty() {
        return Err(format!(
            "No .bin files found in training data directory '{}'",
            data_path.display()
        ));
    }
    let config = TrainingDataConfig {
        num_inputs,
        num_outputs,
    };
    let record_bytes = config.bytes_per_record();
    crate::corpus_guard::assert_records_aligned(&bin_files, record_bytes)?;
    let values_per_record = num_inputs + num_outputs;
    let fused_read_target_bytes =
        crate::read_tuning::training_read_target_bytes_from_env(record_bytes);
    let fused_read_buf_len =
        crate::stream_score::effective_fused_read_buf_len(record_bytes, fused_read_target_bytes);
    let activation_threads = crate::stream_score::activation_worker_count_for_scorer();

    // The partition directory mode gives a one-creature directory, so a
    // creature's cohort sum is bit-identical to scoring it alone.
    let workers = workers_per_creature_split(
        1,
        activation_threads,
        worker_split_factor(),
        crate::host_resources::max_worker_count(&crate::host_resources::host()),
    )[0];
    let n_creatures = plan.len();
    let mut scratches: Vec<CohortScratch> = (0..workers).map(|_| plan.scratch()).collect();
    let mut worker_sums: Vec<Vec<f64>> = vec![vec![0.0; n_creatures]; workers];
    let mut total_loss = vec![0.0_f64; n_creatures];
    let mut total_records = 0_usize;

    let mut score_chunk = |floats: &mut Vec<f32>, n_records: usize| -> Result<(), String> {
        let floats: &[f32] = floats;
        if floats.len() != n_records * values_per_record {
            return Err("Internal float unpack length mismatch".to_string());
        }
        total_records += n_records;
        let actual = workers.min(n_records).max(1);
        let ranges = if actual == 1 {
            std::iter::once(0..floats.len()).collect::<Vec<_>>()
        } else {
            partition_packed_record_ranges(values_per_record, n_records, actual)
        };
        scratches
            .par_iter_mut()
            .zip(worker_sums.par_iter_mut())
            .zip(ranges.par_iter())
            .for_each(|((scratch, sums), range)| {
                sums.fill(0.0);
                plan.mse_sum_into(scratch, &floats[range.clone()], sums);
            });
        // Fold worker partials in worker order, as directory mode does.
        for sums in worker_sums.iter().take(actual) {
            for (t, s) in total_loss.iter_mut().zip(sums) {
                *t += *s;
            }
        }
        Ok(())
    };

    multi_score::training_pass_probe::record_sweep();
    let mut pending: Vec<u8> = Vec::new();
    let mut head = 0_usize;
    let mut unpack_floats: Vec<f32> = Vec::new();
    let mut sampler = sample.sampler();
    for_each_read_chunk(&bin_files, fused_read_buf_len, |chunk| {
        crate::stream_io::run_io_loop(
            chunk,
            &mut pending,
            &mut head,
            &mut unpack_floats,
            record_bytes,
            &mut sampler,
            &mut score_chunk,
        )
    })?;
    if head != pending.len() {
        return Err(format!(
            "Trailing {} bytes (incomplete record) after reading all training files",
            pending.len() - head
        ));
    }
    if total_records == 0 {
        return Err("No training records found".to_string());
    }
    let records_scored = vec![total_records; n_creatures];
    let results = finalise_directory_results(
        &loaded,
        &total_loss,
        &records_scored,
        cost,
        &DirectoryRunMeta {
            training_read_backend: crate::read_tuning::training_read_backend_label().to_string(),
            gpu_backend: crate::gpu::GpuBackendLabel::CpuFallback,
            read_buf_len: fused_read_buf_len,
            activation_threads,
            time_taken_secs: started.elapsed().as_secs_f64(),
            compile_time_secs,
            sample,
        },
    )?;
    Ok(CohortRun::Hosted {
        results,
        stats: plan.stats(),
    })
}
