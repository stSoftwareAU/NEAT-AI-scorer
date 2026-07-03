//! Scoring pipeline for NEAT-AI creatures.
//!
//! Replicates the TypeScript scoring formula from `src/architecture/Score.ts`.
//! Issue #1967 - Build Rust CLI scorer application.

use neat_core::creature::CreatureExport;
use neat_core::squash::SquashType;

use crate::gpu::GpuBackendLabel;

/// The current semantic major version matching the TypeScript constant.
/// Creatures not at this version receive a small penalty.
pub const SEMANTIC_MAJOR_VERSION: u32 = 4;

/// Calculates a penalty value based on the magnitude of a weight or bias.
///
/// Encourages smaller weights/biases. Returns 0 for values <= 1,
/// and approaches (but never reaches) 1 for large values.
/// Uses recursive logarithmic compression for very large values.
///
/// Matches the TypeScript `valuePenalty()` in `src/architecture/Score.ts`.
///
/// Returns `Err` when `value` is negative or non-finite — both reachable from
/// arbitrary creature JSON — so malformed input yields a structured error
/// rather than a release-build panic (Issue #201). The remaining bounds are
/// pure internal-math invariants and stay as `debug_assert!`.
pub fn value_penalty(value: f64) -> Result<f64, String> {
    if value < 0.0 {
        return Err(format!("value_penalty: value {value} is negative"));
    }

    if value <= 1.0 {
        return Ok(0.0);
    }

    if !value.is_finite() {
        return Err(format!("value_penalty: value {value} is not finite"));
    }

    let primary_penalty = 1.0 / (1.0 + 1.0 / value);

    if primary_penalty > 0.999 {
        let compress_penalty = 0.999 + value_penalty(value.ln())? / 1000.0;
        debug_assert!(
            compress_penalty < 1.0,
            "Compressed Penalty: {compress_penalty} is >= 1"
        );
        return Ok(compress_penalty);
    }

    debug_assert!(
        primary_penalty < 1.0,
        "Primary Penalty: {primary_penalty} is >= 1"
    );
    Ok(primary_penalty)
}

/// Calculates the combined weight/bias penalty from max and average absolute values.
fn calculate_penalty(max: f64, avg: f64) -> Result<f64, String> {
    let penalty = (value_penalty(max)? + value_penalty(avg)?) / 2.0;
    debug_assert!(penalty.is_finite(), "Penalty: {penalty} is not finite");
    debug_assert!(penalty >= 0.0, "Penalty: {penalty} is negative");
    debug_assert!(penalty < 1.0, "Penalty: {penalty} is >= 1");
    Ok(penalty)
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
pub fn compute_score_components(creature: &CreatureExport) -> Result<ScoreComponents, String> {
    let mut max_weight_bias: f64 = 0.0;
    let mut total_weight_bias: f64 = 0.0;
    let mut count_weight_bias: usize = 0;
    let mut squash_penalty: f64 = 0.0;

    // Gather weight statistics from synapses
    for synapse in &creature.synapses {
        if !synapse.weight.is_finite() {
            return Err(format!(
                "Synapse weight {} (from {} to {}) is not finite",
                synapse.weight, synapse.from_uuid, synapse.to_uuid
            ));
        }
        let w = synapse.weight.abs();
        if w > max_weight_bias {
            max_weight_bias = w;
        }
        total_weight_bias += w;
        count_weight_bias += 1;
    }

    // Gather bias statistics and squash complexity from non-input neurons
    for neuron in &creature.neurons {
        if !neuron.bias.is_finite() {
            return Err(format!(
                "Neuron bias {} (uuid {}) is not finite",
                neuron.bias, neuron.uuid
            ));
        }
        let b = neuron.bias.abs();
        if b > max_weight_bias {
            max_weight_bias = b;
        }
        total_weight_bias += b;
        count_weight_bias += 1;

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
/// The weight/bias term routes through `calculate_penalty` (which validates
/// finiteness), so every call site shares the same numerical behaviour and the
/// same structured-error propagation (Issue #201).
pub fn complexity_penalty(components: &ScoreComponents, growth_cost: f64) -> Result<f64, String> {
    let weight_bias_penalty =
        calculate_penalty(components.max_weight_bias, components.avg_weight_bias)?;
    let total_penalty = weight_bias_penalty + components.squash_complexity_penalty;

    Ok(components.hidden_neuron_count as f64 * growth_cost
        + components.synapse_count as f64 * growth_cost / 10.0
        + total_penalty * growth_cost / 100.0)
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
pub fn calculate_score(
    error: f64,
    components: &ScoreComponents,
    growth_cost: f64,
    semantic_version: Option<&str>,
) -> Result<f64, String> {
    if error.is_nan() {
        return Err("Average error is NaN".to_string());
    }
    if !error.is_finite() {
        return Err(format!("Average error {error} is not finite"));
    }
    if error < 0.0 {
        return Err(format!("Average error {error} is negative"));
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
    /// Effective `read` buffer size (bytes) after record alignment; set when fused path ran.
    #[serde(rename = "readBufLen", skip_serializing_if = "Option::is_none")]
    pub read_buf_len: Option<usize>,
    /// Fused path only: parallel activation workers when `> 1` (`NEAT_SCORER_ACTIVATION_THREADS`).
    #[serde(rename = "activationThreads", skip_serializing_if = "Option::is_none")]
    pub activation_threads: Option<usize>,
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
        let p = value_penalty(1_000_000.0).unwrap();
        assert!(p < 1.0);
        assert!(p > 0.999);
    }

    #[test]
    fn test_value_penalty_known_values() {
        // valuePenalty(10) = 1 / (1 + 1/10) = 1 / 1.1 ≈ 0.9090909...
        let p = value_penalty(10.0).unwrap();
        assert!((p - 0.909_090_909_090_909_1).abs() < 1e-12);

        // valuePenalty(2) = 1 / (1 + 1/2) = 1/1.5 ≈ 0.6666...
        let p2 = value_penalty(2.0).unwrap();
        assert!((p2 - 2.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn test_value_penalty_compression() {
        // Values large enough to trigger compression (primary_penalty > 0.999)
        // 1/(1+1/v) > 0.999  =>  v > 999
        let p = value_penalty(1000.0).unwrap();
        assert!(p >= 0.999);
        assert!(p < 1.0);

        let p2 = value_penalty(10_000.0).unwrap();
        assert!(p2 >= 0.999);
        assert!(p2 < 1.0);
        assert!(p2 > p);
    }

    #[test]
    fn test_value_penalty_rejects_negative() {
        // Issue #201: a negative value now returns a structured error rather
        // than panicking (previously a `#[should_panic]` test).
        let err = value_penalty(-1.0).unwrap_err();
        assert!(err.contains("negative"), "unexpected message: {err}");
    }

    #[test]
    fn test_value_penalty_rejects_non_finite() {
        // Issue #201: NaN/inf return a structured error instead of panicking.
        let nan = value_penalty(f64::NAN).unwrap_err();
        assert!(nan.contains("not finite"), "unexpected message: {nan}");
        let inf = value_penalty(f64::INFINITY).unwrap_err();
        assert!(inf.contains("not finite"), "unexpected message: {inf}");
    }

    #[test]
    fn test_calculate_penalty_symmetric() {
        let penalty = calculate_penalty(5.0, 2.0).unwrap();
        let expected = (value_penalty(5.0).unwrap() + value_penalty(2.0).unwrap()) / 2.0;
        assert!((penalty - expected).abs() < 1e-12);
    }

    #[test]
    fn test_calculate_penalty_zero_for_small() {
        let penalty = calculate_penalty(0.5, 0.3).unwrap();
        assert_eq!(penalty, 0.0);
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
        };
        let growth_cost = 0.001;

        let cp = complexity_penalty(&components, growth_cost).unwrap();
        assert!(cp > 0.0);

        // v4 creature => zero version penalty, so score = 1 - error - cp.
        let score = calculate_score(0.0, &components, growth_cost, Some("4.0.0")).unwrap();
        assert!((score - (1.0 - cp)).abs() < 1e-12);
    }

    #[test]
    fn test_complexity_penalty_routes_through_calculate_penalty() {
        // The weight/bias term must match calculate_penalty (not a raw
        // value_penalty average) so every call site shares one behaviour.
        let components = ScoreComponents {
            hidden_neuron_count: 4,
            synapse_count: 7,
            max_weight_bias: 5.0,
            avg_weight_bias: 2.0,
            squash_complexity_penalty: 0.0,
        };
        let growth_cost = 0.01;

        let weight_bias_penalty = calculate_penalty(5.0, 2.0).unwrap();
        let total_penalty = weight_bias_penalty + components.squash_complexity_penalty;
        let expected =
            4.0 * growth_cost + 7.0 * growth_cost / 10.0 + total_penalty * growth_cost / 100.0;

        assert!((complexity_penalty(&components, growth_cost).unwrap() - expected).abs() < 1e-12);
    }

    #[test]
    fn test_score_components_from_creature() {
        let creature = CreatureExport {
            input: 2,
            output: 1,
            neurons: vec![
                neat_core::creature::NeuronExport {
                    neuron_type: "hidden".to_string(),
                    uuid: "hidden-0".to_string(),
                    bias: 0.5,
                    squash: Some("TANH".to_string()),
                },
                neat_core::creature::NeuronExport {
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
        }
    }

    #[test]
    fn test_compute_score_components_rejects_non_finite_weight() {
        // Issue #201: a NaN weight from user JSON returns a structured error.
        let nan = compute_score_components(&creature_with(f64::NAN, 0.0)).unwrap_err();
        assert!(nan.contains("Synapse weight"), "unexpected message: {nan}");
        assert!(nan.contains("not finite"), "unexpected message: {nan}");

        let inf = compute_score_components(&creature_with(f64::INFINITY, 0.0)).unwrap_err();
        assert!(inf.contains("not finite"), "unexpected message: {inf}");
    }

    #[test]
    fn test_compute_score_components_rejects_non_finite_bias() {
        // Issue #201: a non-finite bias from user JSON returns a structured error.
        let nan = compute_score_components(&creature_with(0.5, f64::NAN)).unwrap_err();
        assert!(nan.contains("Neuron bias"), "unexpected message: {nan}");
        assert!(nan.contains("not finite"), "unexpected message: {nan}");

        let inf = compute_score_components(&creature_with(0.5, f64::NEG_INFINITY)).unwrap_err();
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
        };
        let nan = calculate_score(f64::NAN, &components, 0.0, Some("4.0.0")).unwrap_err();
        assert!(nan.contains("NaN"), "unexpected message: {nan}");
        let inf = calculate_score(f64::INFINITY, &components, 0.0, Some("4.0.0")).unwrap_err();
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
        };
        let err = calculate_score(-0.1, &components, 0.0, Some("4.0.0")).unwrap_err();
        assert!(err.contains("negative"), "unexpected message: {err}");
    }
}
