//! Scoring pipeline for NEAT-AI creatures.
//!
//! Replicates the TypeScript scoring formula from `src/architecture/Score.ts`.
//! Issue #1967 - Build Rust CLI scorer application.

use neat_core::creature::CreatureExport;
use neat_core::squash::SquashType;

use crate::creature_width::{CreatureWidthError, validate_creature_width};
use crate::gpu::GpuBackendLabel;

/// The current semantic major version matching the TypeScript constant.
/// Creatures not at this version receive a small penalty.
pub(crate) const SEMANTIC_MAJOR_VERSION: u32 = 4;

/// Typed error for the public scoring API (Issue #289, C-GOOD-ERR).
///
/// Replaces the previous stringly-typed `Result<_, String>` contract so callers
/// can `match` on the specific failure (e.g. distinguish a non-finite synapse
/// weight from a negative average error) and compose these errors into their own
/// `Box<dyn std::error::Error>` chains with `?`. Follows the hand-rolled
/// `Display` + `std::error::Error` pattern the GPU module already uses
/// ([`crate::gpu::GpuInitError`]) rather than adding a `thiserror` dependency.
///
/// Every variant's `Display` text is preserved byte-for-byte from the old
/// `format!(...)` error strings so downstream message-matching stays stable.
#[derive(Debug, Clone, PartialEq)]
pub enum ScoringError {
    /// A magnitude passed to [`value_penalty`] was negative.
    NegativeValue {
        /// The offending value.
        value: f64,
    },
    /// A magnitude passed to [`value_penalty`] was non-finite (NaN or infinite).
    NonFiniteValue {
        /// The offending value.
        value: f64,
    },
    /// A synapse weight read from creature JSON was non-finite.
    NonFiniteSynapseWeight {
        /// The offending weight.
        weight: f64,
        /// `fromUUID` of the synapse.
        from: String,
        /// `toUUID` of the synapse.
        to: String,
    },
    /// A neuron bias read from creature JSON was non-finite.
    NonFiniteNeuronBias {
        /// The offending bias.
        bias: f64,
        /// `uuid` of the neuron.
        uuid: String,
    },
    /// The average error passed to [`calculate_score`] was NaN.
    AverageErrorNaN,
    /// The average error passed to [`calculate_score`] was non-finite (infinite).
    NonFiniteAverageError {
        /// The offending average error.
        error: f64,
    },
    /// The average error passed to [`calculate_score`] was negative.
    NegativeAverageError {
        /// The offending average error.
        error: f64,
    },
    /// The creature declared an `input` / `output` count below one (Issue
    /// #571). Wraps the shared [`CreatureWidthError`] so the message is the
    /// same one every scoring path reports.
    InvalidObservationWidth(CreatureWidthError),
}

impl std::fmt::Display for ScoringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NegativeValue { value } => {
                write!(f, "value_penalty: value {value} is negative")
            }
            Self::NonFiniteValue { value } => {
                write!(f, "value_penalty: value {value} is not finite")
            }
            Self::NonFiniteSynapseWeight { weight, from, to } => {
                write!(
                    f,
                    "Synapse weight {weight} (from {from} to {to}) is not finite"
                )
            }
            Self::NonFiniteNeuronBias { bias, uuid } => {
                write!(f, "Neuron bias {bias} (uuid {uuid}) is not finite")
            }
            Self::AverageErrorNaN => f.write_str("Average error is NaN"),
            Self::NonFiniteAverageError { error } => {
                write!(f, "Average error {error} is not finite")
            }
            Self::NegativeAverageError { error } => {
                write!(f, "Average error {error} is negative")
            }
            Self::InvalidObservationWidth(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ScoringError {}

/// Decades of magnitude above 1.0 that span the useful range of a weight or
/// bias. A value at or beyond `10^MAGNITUDE_DECADE_CAP` is treated as fully
/// penalised — nothing in this domain needs a weight that large.
pub const MAGNITUDE_DECADE_CAP: f64 = 12.0;

/// `Number.MAX_SAFE_INTEGER`, the ceiling the TypeScript clamps magnitudes to.
const MAX_SAFE_MAGNITUDE: f64 = 9_007_199_254_740_991.0;

/// Cost of a fully-saturated magnitude penalty, as a multiple of `growth_cost`.
///
/// At the fleet's `growth_cost` of 1e-7 a fully-saturated creature pays 1e-5 of
/// score. Raising this is the single knob that makes magnitude matter more
/// relative to structure.
pub const MAGNITUDE_COST: f64 = 100.0;

/// Calculates a penalty value based on the magnitude of a weight or bias.
///
/// Each **decade** of magnitude above 1.0 costs a constant amount, so growth is
/// never free: `0.999 * log10(v) / MAGNITUDE_DECADE_CAP` up to the cap, then an
/// asymptotic tail that approaches — but never reaches — 1.
///
/// The previous curve, `1 / (1 + 1/v)`, was already 0.990 at `v = 100` and
/// 0.9999 at `v = 1000`, so beyond about two decades it could no longer tell a
/// sensible weight from an absurd one. Weights across the fleet drifted to an
/// average magnitude in the thousands, with individual values reaching 1e+195,
/// because nothing in the score objected.
///
/// Matches the TypeScript `valuePenalty()` in `src/architecture/Score.ts`.
///
/// Returns `Err` when `value` is negative or non-finite — both reachable from
/// arbitrary creature JSON — so malformed input yields a structured error
/// rather than a release-build panic (Issue #201). The remaining bounds are
/// pure internal-math invariants and stay as `debug_assert!`.
///
/// # Examples
///
/// ```
/// use rust_scorer::scoring::value_penalty;
/// // Magnitudes at or below 1.0 carry no penalty.
/// assert_eq!(value_penalty(1.0).unwrap(), 0.0);
/// // Larger magnitudes approach — but never reach — 1.0.
/// let p = value_penalty(10.0).unwrap();
/// assert!(p > 0.0 && p < 1.0);
/// // Every decade costs the same, unlike the old saturating curve.
/// let (a, b) = (value_penalty(10.0).unwrap(), value_penalty(100.0).unwrap());
/// let (c, d) = (value_penalty(1e6).unwrap(), value_penalty(1e7).unwrap());
/// assert!(((b - a) - (d - c)).abs() < 1e-12);
/// // Negative input is rejected.
/// assert!(value_penalty(-1.0).is_err());
/// ```
pub fn value_penalty(value: f64) -> Result<f64, ScoringError> {
    if value < 0.0 {
        return Err(ScoringError::NegativeValue { value });
    }

    if value <= 1.0 {
        return Ok(0.0);
    }

    if !value.is_finite() {
        return Err(ScoringError::NonFiniteValue { value });
    }

    let decades = value.log10();
    let penalty = if decades < MAGNITUDE_DECADE_CAP {
        0.999 * decades / MAGNITUDE_DECADE_CAP
    } else {
        // Asymptotic tail: monotonic, continuous at the cap, always < 1.
        0.999 + 0.001 * (1.0 - MAGNITUDE_DECADE_CAP / decades)
    };

    debug_assert!(penalty >= 0.0, "Penalty: {penalty} is negative");
    debug_assert!(penalty < 1.0, "Penalty: {penalty} is >= 1");
    Ok(penalty)
}

/// Penalty charged for a single weight or bias of absolute value `value`.
///
/// This is the call-site contract around [`value_penalty`]: the magnitude is
/// made absolute and clamped to `MAX_SAFE_MAGNITUDE` first. Individual values
/// reach 1e+195 through compaction multiplications long before any aggregate
/// overflow guard sees them, and the TypeScript `valuePenalty()` rejects
/// anything past that bound outright — so clamping here is what keeps the two
/// engines returning the same number for the magnitudes production actually
/// produces (NEAT-AI#3881).
///
/// Matches the TypeScript `magnitudePenalty()` in `src/architecture/Score.ts`.
/// Both engines are pinned to the shared corpus in
/// `rust_scorer/tests/fixtures/magnitude-penalty-corpus.json`.
///
/// # Examples
///
/// ```
/// use rust_scorer::scoring::magnitude_penalty;
/// // Sign is ignored.
/// assert_eq!(magnitude_penalty(-100.0).unwrap(), magnitude_penalty(100.0).unwrap());
/// // A magnitude past the safe bound is charged, not rejected.
/// let clamped = magnitude_penalty(1.1559466326634707e195).unwrap();
/// assert!(clamped < 1.0);
/// assert_eq!(clamped, magnitude_penalty(9_007_199_254_740_991.0).unwrap());
/// ```
pub fn magnitude_penalty(value: f64) -> Result<f64, ScoringError> {
    if value.is_nan() {
        return Err(ScoringError::NonFiniteValue { value });
    }
    value_penalty(value.abs().min(MAX_SAFE_MAGNITUDE))
}

/// Returns the squash function complexity penalty for a given squash type.
///
/// Only the IF aggregate function carries a non-zero complexity penalty (3).
/// All other activation functions have zero complexity penalty.
fn squash_complexity_penalty(squash_type: SquashType) -> f64 {
    match squash_type {
        SquashType::If => 3.0,
        _ => 0.0,
    }
}

/// Components extracted from a creature for score calculation.
#[derive(Debug)]
pub struct ScoreComponents {
    /// Number of hidden neurons (excluding input and output).
    pub hidden_neuron_count: usize,
    /// Total number of synapses.
    pub synapse_count: usize,
    /// Maximum absolute weight or bias value.
    pub max_weight_bias: f64,
    /// Average absolute weight or bias value.
    pub avg_weight_bias: f64,
    /// Sum of squash function complexity penalties for all non-input neurons.
    pub squash_complexity_penalty: f64,
    /// Mean of [`value_penalty`] over **every** weight and bias.
    ///
    /// The magnitude term reads this rather than `(max, avg)`. Penalising the
    /// max and the average gave the other ~38,000 values in a production
    /// creature no gradient at all: growing a typical weight by a full decade
    /// moved the score by ~1e-12, so evolution shaved the single largest weight
    /// and let the body of the distribution drift. Averaging the per-value
    /// penalty gives every weight and bias an equal say.
    pub mean_value_penalty: f64,
}

/// Computes score components from a creature export.
///
/// Iterates over synapses and neurons to gather weight/bias statistics
/// and squash complexity penalties.
///
/// Returns `Err` when any synapse weight or neuron bias is non-finite. These
/// values come straight from user-supplied creature JSON, so a NaN/inf weight
/// or bias yields a structured error instead of a release-build panic
/// (Issue #201).
///
/// # Examples
///
/// ```
/// use rust_scorer::scoring::compute_score_components;
/// use neat_core::creature::{CreatureExport, NeuronExport, SynapseExport};
///
/// let creature = CreatureExport {
///     input: 1,
///     output: 1,
///     neurons: vec![NeuronExport {
///         id: None,
///         neuron_type: "output".to_string(),
///         uuid: "output-0".to_string(),
///         bias: -0.3,
///         squash: Some("IDENTITY".to_string()),
///     }],
///     synapses: vec![SynapseExport {
///         from_uuid: "input-0".to_string(),
///         to_uuid: "output-0".to_string(),
///         weight: 2.0,
///         synapse_type: None,
///     }],
///     semantic_version: Some("4.0.0".to_string()),
///     forward_only: true,
///     memetic: None,
/// };
///
/// let components = compute_score_components(&creature).unwrap();
/// assert_eq!(components.synapse_count, 1);
/// assert_eq!(components.hidden_neuron_count, 0);
/// // Max absolute weight/bias across `{2.0, 0.3}` is `2.0`.
/// assert!((components.max_weight_bias - 2.0).abs() < 1e-12);
/// ```
pub fn compute_score_components(
    creature: &CreatureExport,
) -> Result<ScoreComponents, ScoringError> {
    // Issue #571: a widthless creature is rejected on every path, including
    // this data-free complexity pass.
    validate_creature_width(creature).map_err(ScoringError::InvalidObservationWidth)?;

    let mut max_weight_bias: f64 = 0.0;
    let mut total_weight_bias: f64 = 0.0;
    let mut count_weight_bias: usize = 0;
    let mut squash_penalty: f64 = 0.0;
    let mut sum_value_penalty: f64 = 0.0;

    // Gather weight statistics from synapses
    for synapse in &creature.synapses {
        if !synapse.weight.is_finite() {
            return Err(ScoringError::NonFiniteSynapseWeight {
                weight: synapse.weight,
                from: synapse.from_uuid.clone(),
                to: synapse.to_uuid.clone(),
            });
        }
        let w = synapse.weight.abs();
        if w > max_weight_bias {
            max_weight_bias = w;
        }
        total_weight_bias += w;
        count_weight_bias += 1;
        sum_value_penalty += magnitude_penalty(w)?;
    }

    // Gather bias statistics and squash complexity from non-input neurons
    for neuron in &creature.neurons {
        if !neuron.bias.is_finite() {
            return Err(ScoringError::NonFiniteNeuronBias {
                bias: neuron.bias,
                uuid: neuron.uuid.clone(),
            });
        }
        let b = neuron.bias.abs();
        if b > max_weight_bias {
            max_weight_bias = b;
        }
        total_weight_bias += b;
        count_weight_bias += 1;
        sum_value_penalty += magnitude_penalty(b)?;

        // Accumulate squash complexity
        let squash_name = neuron.squash.as_deref().unwrap_or("IDENTITY");
        if let Ok(squash_type) = neat_core::creature::parse_squash_name(squash_name) {
            squash_penalty += squash_complexity_penalty(squash_type);
        }
    }

    // Overflow protection matching TypeScript behaviour
    let safe_max = if max_weight_bias > 9_007_199_254_740_992.0 {
        9_007_199_254_740_992.0 // Number.MAX_SAFE_INTEGER
    } else {
        max_weight_bias
    };
    let safe_total = if total_weight_bias > 9_007_199_254_740_992.0 {
        9_007_199_254_740_992.0
    } else {
        total_weight_bias
    };

    let avg_weight_bias = if count_weight_bias > 0 {
        safe_total / count_weight_bias as f64
    } else {
        0.0
    };

    let mean_value_penalty = if count_weight_bias > 0 {
        sum_value_penalty / count_weight_bias as f64
    } else {
        0.0
    };

    let hidden_neuron_count = creature
        .neurons
        .iter()
        .filter(|n| n.neuron_type == "hidden" || n.neuron_type == "constant")
        .count();

    Ok(ScoreComponents {
        hidden_neuron_count,
        synapse_count: creature.synapses.len(),
        max_weight_bias: safe_max,
        avg_weight_bias,
        squash_complexity_penalty: squash_penalty,
        mean_value_penalty,
    })
}

/// Canonical complexity-penalty formula shared across every result-assembly site.
///
/// Returns the growth-cost-weighted penalty that `calculate_score` subtracts and
/// that callers report as the JSON `complexityPenalty` field. Keeping a single
/// implementation guarantees the reported penalty can never silently disagree
/// with the value baked into `score`.
///
/// `complexityPenalty = hiddenNeurons * growthCost + synapses * growthCost / 10
///                      + (weightBiasPenalty + squashComplexityPenalty) * growthCost / 100`
///
/// The magnitude term is `meanValuePenalty * growthCost * MAGNITUDE_COST` and
/// carries its own coefficient rather than sharing the squash term's `/100`,
/// which is what previously capped the whole magnitude contribution at 1e-9 of
/// score on the fleet's `growth_cost`.
///
/// # Examples
///
/// ```
/// use rust_scorer::scoring::{ScoreComponents, complexity_penalty};
///
/// let components = ScoreComponents {
///     hidden_neuron_count: 2,
///     synapse_count: 4,
///     max_weight_bias: 1.5,
///     avg_weight_bias: 0.75,
///     squash_complexity_penalty: 0.0,
///     mean_value_penalty: 0.0,
/// };
/// let penalty = complexity_penalty(&components, 0.01).unwrap();
/// assert!(penalty > 0.0);
/// ```
pub fn complexity_penalty(
    components: &ScoreComponents,
    growth_cost: f64,
) -> Result<f64, ScoringError> {
    let penalty = components.hidden_neuron_count as f64 * growth_cost
        + components.synapse_count as f64 * growth_cost / 10.0
        + components.squash_complexity_penalty * growth_cost / 100.0
        + components.mean_value_penalty * growth_cost * MAGNITUDE_COST;

    debug_assert!(
        penalty.is_finite(),
        "Complexity penalty {penalty} is not finite"
    );
    debug_assert!(penalty >= 0.0, "Complexity penalty {penalty} is negative");
    Ok(penalty)
}

/// Calculates the final score for a creature.
///
/// Formula: `score = 1 - error - complexityPenalty - versionPenalty`
///
/// Where:
/// - `complexityPenalty` is computed by [`complexity_penalty`].
/// - `versionPenalty = 1e-6` if the creature's semantic version doesn't start with "4."
///
/// Returns `Err` when `error` is non-finite or negative. The average error is
/// derived from the chosen cost over user-supplied training data, so a cost
/// that yields a non-finite or negative average produces a structured error
/// rather than a release-build panic (Issue #201). The `score <= 1.0` bound is
/// a pure internal-math invariant (guaranteed once `error >= 0` and the
/// penalties are non-negative) and stays as `debug_assert!`.
///
/// # Examples
///
/// ```
/// use rust_scorer::scoring::{ScoreComponents, calculate_score};
///
/// let components = ScoreComponents {
///     hidden_neuron_count: 0,
///     synapse_count: 0,
///     max_weight_bias: 0.0,
///     avg_weight_bias: 0.0,
///     squash_complexity_penalty: 0.0,
///     mean_value_penalty: 0.0,
/// };
/// // Zero error, no complexity, current major version → a perfect score of 1.0.
/// let score = calculate_score(0.0, &components, 0.01, Some("4.1.0")).unwrap();
/// assert!((score - 1.0).abs() < 1e-12);
/// // An off-version creature pays a tiny version penalty.
/// assert!(calculate_score(0.0, &components, 0.01, Some("3.0.0")).unwrap() < 1.0);
/// // A negative average error is rejected.
/// assert!(calculate_score(-0.1, &components, 0.01, None).is_err());
/// ```
pub fn calculate_score(
    error: f64,
    components: &ScoreComponents,
    growth_cost: f64,
    semantic_version: Option<&str>,
) -> Result<f64, ScoringError> {
    if error.is_nan() {
        return Err(ScoringError::AverageErrorNaN);
    }
    if !error.is_finite() {
        return Err(ScoringError::NonFiniteAverageError { error });
    }
    if error < 0.0 {
        return Err(ScoringError::NegativeAverageError { error });
    }

    let complexity_penalty = complexity_penalty(components, growth_cost)?;

    let version_penalty = match semantic_version {
        Some(v) if v.starts_with(&format!("{SEMANTIC_MAJOR_VERSION}.")) => 0.0,
        _ => 1e-6,
    };

    let score = 1.0 - error - complexity_penalty - version_penalty;
    debug_assert!(score <= 1.0, "Score: {score} is greater than 1");
    Ok(score)
}

/// Result of scoring a creature against training data.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreResult {
    /// Final fitness score (`1.0 - error - complexity_penalty - version_penalty`).
    pub score: f64,
    /// Mean cost/error of the creature over the training records.
    pub error: f64,
    /// Penalty applied for network complexity (neurons + synapses).
    pub complexity_penalty: f64,
    /// Number of training records scored.
    pub record_count: usize,
    /// Count of hidden neurons in the creature.
    pub hidden_neurons: usize,
    /// Count of synapses in the creature.
    pub synapse_count: usize,
    /// Creature JSON `forwardOnly` — must be `true` for fused + pipelined `.bin` scoring.
    #[serde(rename = "forwardOnly")]
    pub forward_only: bool,
    /// Which training read path ran ([`crate::read_tuning::training_read_backend_label`] when fused), else `record_iterator`.
    #[serde(rename = "trainingReadBackend")]
    pub training_read_backend: String,
    /// GPU backend selected for this run (Issue #80). `cpu-fallback` when
    /// `--gpu off` (the default) or when `--gpu auto` finds no compatible
    /// adapter. Real native labels (`metal`, `vulkan`, `dx12`, `gl`) appear
    /// once GPU kernels start consuming the device in #81.
    #[serde(rename = "gpuBackend")]
    pub gpu_backend: GpuBackendLabel,
    /// Effective `read` buffer size (bytes) after record alignment; set when the
    /// fused path ran.
    ///
    /// Issue #549: **per file reader**. With `fileReadWorkers > 1` each reader
    /// holds one buffer of this size, and the aggregate stays inside the host's
    /// read budget — before #549 this reported the unsplit record-size tier,
    /// which overstated what the readers actually held.
    #[serde(rename = "readBufLen", skip_serializing_if = "Option::is_none")]
    pub read_buf_len: Option<usize>,
    /// Fused path only: parallel activation workers **per file reader** when
    /// `> 1` (`NEAT_SCORER_ACTIVATION_THREADS`, divided by `fileReadWorkers`).
    #[serde(rename = "activationThreads", skip_serializing_if = "Option::is_none")]
    pub activation_threads: Option<usize>,
    /// Issue #529 — fused path only: `.bin` files read and scored concurrently
    /// when `> 1` (`NEAT_SCORER_FILE_THREADS`). Absent for a single sequential
    /// reader, so existing consumers see no new field on a one-file corpus.
    #[serde(rename = "fileReadWorkers", skip_serializing_if = "Option::is_none")]
    pub file_read_workers: Option<usize>,
    /// Fused path + `activationThreads` > 1: how many pending-buffer batches actually ran via Rayon (`0` = still sequential).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_activation_batches: Option<usize>,
    /// Fused path + `activationThreads` > 1: max whole records in one activation batch (if `1` with zero parallel batches, widen reads).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_activation_batch_records: Option<usize>,
    /// Wall-clock seconds for the full scoring run (read creature, compile, evaluate data).
    #[serde(rename = "timeTaken")]
    pub time_taken_secs: f64,
    /// Wall-clock seconds spent in `compile_creature` (and any per-worker clone) before scoring.
    /// Issue #42: useful for diagnosing how much of `timeTaken` is fixed startup vs scoring.
    #[serde(rename = "compileTimeSecs", skip_serializing_if = "Option::is_none")]
    pub compile_time_secs: Option<f64>,
    /// Issue #121: the resolved cost-function name that scoring actually
    /// applied (one of `BUILT_IN_COST_NAMES`). Lets the TS bridge confirm
    /// `--cost` was honoured by the scorer rather than silently downgraded
    /// to MSE.
    #[serde(rename = "costName")]
    pub cost_name: String,
    /// Issue #82: name of the GPU compute kernel used for this run (e.g.
    /// `"forward_mse_batched"`). Absent when the run executed on CPU.
    #[serde(rename = "gpuKernel", skip_serializing_if = "Option::is_none")]
    pub gpu_kernel: Option<String>,
    /// Issue #82: cap on chunks held in flight on the GPU at any one time.
    /// `1` means no pipelining (current chunk's read blocks until the previous
    /// chunk's compute finishes); `2` means double-buffered I/O.
    #[serde(rename = "gpuInflightChunks", skip_serializing_if = "Option::is_none")]
    pub gpu_inflight_chunks: Option<usize>,
    /// Issue #82: number of `dispatch_workgroups` calls the GPU runner made
    /// across the whole corpus (one per chunk on the multi-creature path).
    #[serde(rename = "gpuDispatchCount", skip_serializing_if = "Option::is_none")]
    pub gpu_dispatch_count: Option<usize>,
    /// Issue #310: the effective record-level sub-sampling rate applied by the
    /// forward-only streaming reader (`--sample-rate`). Present (and in
    /// `(0, 1)`) only when sub-sampling actually ran, so a consumer can confirm
    /// the scorer honoured `--sample-rate` rather than silently ignoring it, and
    /// read `record_count` as the number of *sampled* records scored. Absent for
    /// a full-corpus run (`--sample-rate 1`, the default).
    #[serde(rename = "sampleRate", skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_penalty_zero_for_small_values() {
        assert_eq!(value_penalty(0.0).unwrap(), 0.0);
        assert_eq!(value_penalty(0.5).unwrap(), 0.0);
        assert_eq!(value_penalty(1.0).unwrap(), 0.0);
    }

    #[test]
    fn test_value_penalty_increases_with_magnitude() {
        let p2 = value_penalty(2.0).unwrap();
        let p10 = value_penalty(10.0).unwrap();
        let p100 = value_penalty(100.0).unwrap();

        assert!(p2 > 0.0);
        assert!(p10 > p2);
        assert!(p100 > p10);
    }

    #[test]
    fn test_value_penalty_never_reaches_one() {
        // Inside the linear region a value of 1e6 sits exactly half way to the
        // 12-decade cap. The old curve put it at 0.99993, indistinguishable
        // from 1e195.
        let p = value_penalty(1_000_000.0).unwrap();
        assert!(p < 1.0);
        assert!(
            (p - 0.999 * 6.0 / MAGNITUDE_DECADE_CAP).abs() < 1e-12,
            "1e6 is 6 decades up, got {p}"
        );
    }

    #[test]
    fn test_value_penalty_known_values() {
        // valuePenalty(v) = 0.999 * log10(v) / 12 inside the linear region.
        let p = value_penalty(10.0).unwrap();
        assert!((p - 0.999 / MAGNITUDE_DECADE_CAP).abs() < 1e-12, "got {p}");

        let p2 = value_penalty(2.0).unwrap();
        assert!(
            (p2 - 0.999 * 2.0_f64.log10() / MAGNITUDE_DECADE_CAP).abs() < 1e-12,
            "got {p2}"
        );
    }

    #[test]
    fn test_value_penalty_compression() {
        // Beyond the decade cap the asymptotic tail keeps the penalty under 1
        // while still rising, so an absurd magnitude is never free.
        let p = value_penalty(1e12).unwrap();
        assert!(
            p >= 0.999,
            "at the cap the penalty should be ~0.999, got {p}"
        );
        assert!(p < 1.0);

        let p2 = value_penalty(1e195).unwrap();
        assert!(p2 < 1.0, "penalty must never reach 1, got {p2}");
        assert!(p2 > p, "the tail must stay strictly increasing");
    }

    #[test]
    fn test_value_penalty_rejects_negative() {
        // Issue #201: a negative value now returns a structured error rather
        // than panicking (previously a `#[should_panic]` test).
        // Issue #289: the error is now the typed `ScoringError`; assert on its
        // `Display` text so the historical message contract is preserved.
        let err = value_penalty(-1.0).unwrap_err().to_string();
        assert!(err.contains("negative"), "unexpected message: {err}");
    }

    #[test]
    fn test_value_penalty_rejects_non_finite() {
        // Issue #201: NaN/inf return a structured error instead of panicking.
        // Issue #289: assert on the typed error's `Display` text.
        let nan = value_penalty(f64::NAN).unwrap_err().to_string();
        assert!(nan.contains("not finite"), "unexpected message: {nan}");
        let inf = value_penalty(f64::INFINITY).unwrap_err().to_string();
        assert!(inf.contains("not finite"), "unexpected message: {inf}");
    }

    #[test]
    fn test_value_penalty_costs_the_same_per_decade() {
        // The property the old `1/(1 + 1/v)` curve lacked: it was 0.990 at 100
        // and 0.9999 at 1000, so past two decades growth was effectively free.
        let step_low = value_penalty(100.0).unwrap() - value_penalty(10.0).unwrap();
        let step_mid = value_penalty(1e6).unwrap() - value_penalty(1e5).unwrap();
        let step_high = value_penalty(1e11).unwrap() - value_penalty(1e10).unwrap();
        assert!(
            (step_low - step_mid).abs() < 1e-12 && (step_mid - step_high).abs() < 1e-12,
            "decades must cost equally: {step_low} {step_mid} {step_high}"
        );
    }

    #[test]
    fn test_value_penalty_discriminates_where_the_fleet_lives() {
        // Published creatures carry avg|w| in the thousands and max|w| ~1e8.
        // The old curve scored those 0.9999 and 0.99999999 — indistinguishable.
        let avg = value_penalty(4_544.0).unwrap();
        let max = value_penalty(1.631e8).unwrap();
        assert!(
            max - avg > 0.3,
            "the fleet's avg and max magnitudes must be clearly separated, got {avg} vs {max}"
        );
    }

    #[test]
    fn test_value_penalty_zero_for_small() {
        assert_eq!(value_penalty(0.5).unwrap(), 0.0);
        assert_eq!(value_penalty(1.0).unwrap(), 0.0);
    }

    #[test]
    fn test_squash_complexity_if_has_penalty() {
        assert_eq!(squash_complexity_penalty(SquashType::If), 3.0);
    }

    #[test]
    fn test_squash_complexity_standard_zero() {
        assert_eq!(squash_complexity_penalty(SquashType::Relu), 0.0);
        assert_eq!(squash_complexity_penalty(SquashType::Tanh), 0.0);
        assert_eq!(squash_complexity_penalty(SquashType::Logistic), 0.0);
        assert_eq!(squash_complexity_penalty(SquashType::Identity), 0.0);
    }

    #[test]
    fn test_calculate_score_perfect() {
        let components = ScoreComponents {
            hidden_neuron_count: 0,
            synapse_count: 2,
            max_weight_bias: 0.5,
            avg_weight_bias: 0.3,
            squash_complexity_penalty: 0.0,
            mean_value_penalty: 0.0,
        };
        // Error=0, version matched => score close to 1 (minus small synapse penalty)
        let score = calculate_score(0.0, &components, 0.0001, Some("4.0.0")).unwrap();
        assert!(score > 0.99);
        assert!(score <= 1.0);
    }

    #[test]
    fn test_calculate_score_with_error() {
        let components = ScoreComponents {
            hidden_neuron_count: 0,
            synapse_count: 0,
            max_weight_bias: 0.5,
            avg_weight_bias: 0.3,
            squash_complexity_penalty: 0.0,
            mean_value_penalty: 0.0,
        };
        let score = calculate_score(0.5, &components, 0.0, Some("4.0.0")).unwrap();
        assert!((score - 0.5).abs() < 1e-12);
    }

    #[test]
    fn test_version_penalty_applied() {
        let components = ScoreComponents {
            hidden_neuron_count: 0,
            synapse_count: 0,
            max_weight_bias: 0.5,
            avg_weight_bias: 0.3,
            squash_complexity_penalty: 0.0,
            mean_value_penalty: 0.0,
        };
        let score_v4 = calculate_score(0.0, &components, 0.0, Some("4.1.0")).unwrap();
        let score_v3 = calculate_score(0.0, &components, 0.0, Some("3.0.0")).unwrap();
        let score_none = calculate_score(0.0, &components, 0.0, None).unwrap();

        assert!((score_v4 - 1.0).abs() < 1e-12);
        assert!((score_v3 - (1.0 - 1e-6)).abs() < 1e-12);
        assert!((score_none - (1.0 - 1e-6)).abs() < 1e-12);
    }

    #[test]
    fn test_complexity_penalty_formula() {
        let components = ScoreComponents {
            hidden_neuron_count: 10,
            synapse_count: 50,
            max_weight_bias: 0.5,
            avg_weight_bias: 0.3,
            squash_complexity_penalty: 0.0,
            mean_value_penalty: 0.0,
        };
        let growth_cost = 0.001;
        // Expected: 10 * 0.001 + 50 * 0.001 / 10 + 0 * 0.001 / 100
        //         = 0.01 + 0.005 + 0 = 0.015
        let score = calculate_score(0.0, &components, growth_cost, Some("4.0.0")).unwrap();
        assert!((score - (1.0 - 0.015)).abs() < 1e-12);
    }

    #[test]
    fn test_complexity_penalty_matches_calculate_score() {
        // Issue #199: the standalone shared function must produce exactly the
        // penalty baked into the score by calculate_score.
        let components = ScoreComponents {
            hidden_neuron_count: 10,
            synapse_count: 50,
            max_weight_bias: 2.0,
            avg_weight_bias: 1.5,
            squash_complexity_penalty: 3.0,
            mean_value_penalty: 0.0,
        };
        let growth_cost = 0.001;

        let cp = complexity_penalty(&components, growth_cost).unwrap();
        assert!(cp > 0.0);

        // v4 creature => zero version penalty, so score = 1 - error - cp.
        let score = calculate_score(0.0, &components, growth_cost, Some("4.0.0")).unwrap();
        assert!((score - (1.0 - cp)).abs() < 1e-12);
    }

    #[test]
    fn test_complexity_penalty_charges_the_magnitude_term() {
        // The magnitude term carries its own coefficient rather than sharing
        // the squash term's /100, which is what made it worth at most 1e-9 of
        // score at the fleet's growth_cost.
        let components = ScoreComponents {
            hidden_neuron_count: 4,
            synapse_count: 7,
            max_weight_bias: 5.0,
            avg_weight_bias: 2.0,
            squash_complexity_penalty: 3.0,
            mean_value_penalty: 0.25,
        };
        let growth_cost = 0.01;

        let expected = 4.0 * growth_cost
            + 7.0 * growth_cost / 10.0
            + 3.0 * growth_cost / 100.0
            + 0.25 * growth_cost * MAGNITUDE_COST;

        assert!((complexity_penalty(&components, growth_cost).unwrap() - expected).abs() < 1e-12);
    }

    #[test]
    fn test_magnitude_penalty_gives_every_value_a_gradient() {
        // `(max, avg)` gave a typical weight in a 38k-value creature a gradient
        // of ~1e-12 per decade. The mean over all values must move for ANY
        // value growing, not just the largest.
        use neat_core::creature::{CreatureExport, NeuronExport, SynapseExport};
        let build = |w: f64| CreatureExport {
            input: 2,
            output: 1,
            neurons: vec![NeuronExport {
                id: None,
                neuron_type: "output".to_string(),
                uuid: "output-0".to_string(),
                bias: 0.1,
                squash: Some("IDENTITY".to_string()),
            }],
            synapses: vec![
                SynapseExport {
                    from_uuid: "input-0".to_string(),
                    to_uuid: "output-0".to_string(),
                    weight: 1e6,
                    synapse_type: None,
                },
                SynapseExport {
                    from_uuid: "input-1".to_string(),
                    to_uuid: "output-0".to_string(),
                    weight: w,
                    synapse_type: None,
                },
            ],
            semantic_version: Some("4.0.0".to_string()),
            forward_only: true,
            memetic: None,
        };
        // `w` is never the maximum, so under the old (max, avg) aggregate its
        // decade of growth barely registered.
        let small = compute_score_components(&build(10.0)).unwrap();
        let bigger = compute_score_components(&build(100.0)).unwrap();
        assert!(
            bigger.mean_value_penalty > small.mean_value_penalty,
            "a non-maximal value growing a decade must raise the penalty: {} vs {}",
            small.mean_value_penalty,
            bigger.mean_value_penalty
        );
    }

    #[test]
    fn test_score_components_from_creature() {
        let creature = CreatureExport {
            input: 2,
            output: 1,
            neurons: vec![
                neat_core::creature::NeuronExport {
                    id: None,
                    neuron_type: "hidden".to_string(),
                    uuid: "hidden-0".to_string(),
                    bias: 0.5,
                    squash: Some("TANH".to_string()),
                },
                neat_core::creature::NeuronExport {
                    id: None,
                    neuron_type: "output".to_string(),
                    uuid: "output-0".to_string(),
                    bias: -0.3,
                    squash: Some("LOGISTIC".to_string()),
                },
            ],
            synapses: vec![
                neat_core::creature::SynapseExport {
                    from_uuid: "input-0".to_string(),
                    to_uuid: "hidden-0".to_string(),
                    weight: 1.5,
                    synapse_type: None,
                },
                neat_core::creature::SynapseExport {
                    from_uuid: "input-1".to_string(),
                    to_uuid: "hidden-0".to_string(),
                    weight: -0.8,
                    synapse_type: None,
                },
                neat_core::creature::SynapseExport {
                    from_uuid: "hidden-0".to_string(),
                    to_uuid: "output-0".to_string(),
                    weight: 2.0,
                    synapse_type: None,
                },
            ],
            semantic_version: Some("4.1.0".to_string()),
            forward_only: false,
            memetic: None,
        };

        let components = compute_score_components(&creature).unwrap();
        assert_eq!(components.hidden_neuron_count, 1);
        assert_eq!(components.synapse_count, 3);
        // Max abs: max(1.5, 0.8, 2.0, 0.5, 0.3) = 2.0
        assert!((components.max_weight_bias - 2.0).abs() < 1e-12);
        // Total abs: 1.5 + 0.8 + 2.0 + 0.5 + 0.3 = 5.1, count = 5
        assert!((components.avg_weight_bias - 5.1 / 5.0).abs() < 1e-12);
        assert_eq!(components.squash_complexity_penalty, 0.0);
    }

    /// Builds a single-neuron, single-synapse creature for error-path tests.
    fn creature_with(weight: f64, bias: f64) -> CreatureExport {
        CreatureExport {
            input: 1,
            output: 1,
            neurons: vec![neat_core::creature::NeuronExport {
                id: None,
                neuron_type: "output".to_string(),
                uuid: "output-0".to_string(),
                bias,
                squash: Some("IDENTITY".to_string()),
            }],
            synapses: vec![neat_core::creature::SynapseExport {
                from_uuid: "input-0".to_string(),
                to_uuid: "output-0".to_string(),
                weight,
                synapse_type: None,
            }],
            semantic_version: Some("4.0.0".to_string()),
            forward_only: false,
            memetic: None,
        }
    }

    #[test]
    fn test_compute_score_components_rejects_non_finite_weight() {
        // Issue #201: a NaN weight from user JSON returns a structured error.
        // Issue #289: assert on the typed error's `Display` text.
        let nan = compute_score_components(&creature_with(f64::NAN, 0.0))
            .unwrap_err()
            .to_string();
        assert!(nan.contains("Synapse weight"), "unexpected message: {nan}");
        assert!(nan.contains("not finite"), "unexpected message: {nan}");

        let inf = compute_score_components(&creature_with(f64::INFINITY, 0.0))
            .unwrap_err()
            .to_string();
        assert!(inf.contains("not finite"), "unexpected message: {inf}");
    }

    #[test]
    fn test_compute_score_components_rejects_non_finite_bias() {
        // Issue #201: a non-finite bias from user JSON returns a structured error.
        // Issue #289: assert on the typed error's `Display` text.
        let nan = compute_score_components(&creature_with(0.5, f64::NAN))
            .unwrap_err()
            .to_string();
        assert!(nan.contains("Neuron bias"), "unexpected message: {nan}");
        assert!(nan.contains("not finite"), "unexpected message: {nan}");

        let inf = compute_score_components(&creature_with(0.5, f64::NEG_INFINITY))
            .unwrap_err()
            .to_string();
        assert!(inf.contains("not finite"), "unexpected message: {inf}");
    }

    #[test]
    fn test_calculate_score_rejects_non_finite_error() {
        // Issue #201: a non-finite average error returns a structured error
        // rather than panicking in release builds.
        let components = ScoreComponents {
            hidden_neuron_count: 0,
            synapse_count: 0,
            max_weight_bias: 0.5,
            avg_weight_bias: 0.3,
            squash_complexity_penalty: 0.0,
            mean_value_penalty: 0.0,
        };
        // Issue #289: assert on the typed error's `Display` text.
        let nan = calculate_score(f64::NAN, &components, 0.0, Some("4.0.0"))
            .unwrap_err()
            .to_string();
        assert!(nan.contains("NaN"), "unexpected message: {nan}");
        let inf = calculate_score(f64::INFINITY, &components, 0.0, Some("4.0.0"))
            .unwrap_err()
            .to_string();
        assert!(inf.contains("not finite"), "unexpected message: {inf}");
    }

    #[test]
    fn test_calculate_score_rejects_negative_error() {
        // Issue #201: a negative average error returns a structured error.
        let components = ScoreComponents {
            hidden_neuron_count: 0,
            synapse_count: 0,
            max_weight_bias: 0.5,
            avg_weight_bias: 0.3,
            squash_complexity_penalty: 0.0,
            mean_value_penalty: 0.0,
        };
        // Issue #289: assert on the typed error's `Display` text.
        let err = calculate_score(-0.1, &components, 0.0, Some("4.0.0"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("negative"), "unexpected message: {err}");
    }

    #[test]
    fn scoring_error_matches_specific_variants() {
        // Issue #289 (C-GOOD-ERR): callers can `match` on the discriminant to
        // react differently instead of parsing a free-text string.
        assert_eq!(
            value_penalty(-2.0).unwrap_err(),
            ScoringError::NegativeValue { value: -2.0 }
        );
        assert_eq!(
            value_penalty(f64::INFINITY).unwrap_err(),
            ScoringError::NonFiniteValue {
                value: f64::INFINITY
            }
        );

        match compute_score_components(&creature_with(f64::NAN, 0.0)).unwrap_err() {
            ScoringError::NonFiniteSynapseWeight { from, to, .. } => {
                assert_eq!(from, "input-0");
                assert_eq!(to, "output-0");
            }
            other => panic!("expected NonFiniteSynapseWeight, got {other:?}"),
        }
        match compute_score_components(&creature_with(0.5, f64::NAN)).unwrap_err() {
            ScoringError::NonFiniteNeuronBias { uuid, .. } => assert_eq!(uuid, "output-0"),
            other => panic!("expected NonFiniteNeuronBias, got {other:?}"),
        }

        let components = ScoreComponents {
            hidden_neuron_count: 0,
            synapse_count: 0,
            max_weight_bias: 0.5,
            avg_weight_bias: 0.3,
            squash_complexity_penalty: 0.0,
            mean_value_penalty: 0.0,
        };
        assert_eq!(
            calculate_score(f64::NAN, &components, 0.0, None).unwrap_err(),
            ScoringError::AverageErrorNaN
        );
        assert_eq!(
            calculate_score(-0.5, &components, 0.0, None).unwrap_err(),
            ScoringError::NegativeAverageError { error: -0.5 }
        );
    }

    #[test]
    fn scoring_error_composes_as_std_error() {
        // Issue #289: the typed error implements `std::error::Error`, so it can
        // flow into a caller's `Box<dyn Error>` chain via `?`.
        fn caller() -> Result<f64, Box<dyn std::error::Error>> {
            Ok(value_penalty(-1.0)?)
        }
        let err = caller().unwrap_err();
        assert!(err.to_string().contains("negative"));
        // Downcast confirms the concrete typed error survives boxing.
        assert!(err.downcast_ref::<ScoringError>().is_some());
    }
}
