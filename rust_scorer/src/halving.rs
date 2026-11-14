//! Issue #680 — opt-in experiment: current screening versus cached successive
//! halving.
//!
//! The ordinary scorer CLI does not call this module. The opt-in experiment
//! compares four arms on the same candidate population and corpus:
//!
//! * [`run_full`] scores every candidate on every record.
//! * [`run_independent_screening`] applies an experimental halving controller
//!   to the existing [`SampleSpec`] stride, re-scoring each rung from zero.
//!   The scorer itself supplies sampling and caller-directed early exit, not
//!   this promotion schedule.
//! * [`run_nested_uncached`] uses the same nested records as cached halving
//!   but re-scores cumulative subsets, isolating the effect of caching.
//! * [`run_cached_halving`] is the **new** mechanism: deterministic *nested*
//!   strata ([`record_rank`], [`rung_increments`]) so record IDs are shared
//!   across candidates and rungs, and each rung evaluates only the records the
//!   candidate has not yet seen.
//!
//! Cached sufficient statistics ([`SufficientStats`]: error sum plus record
//! count) are valid because [`accumulate_cost_sum`] is a per-record SUM for
//! every built-in cost, i.e. the loss is decomposable over records. The final
//! rung is the full corpus, so the cached result must match a direct full
//! evaluation within [`FULL_MATCH_REL_TOLERANCE`] ([`verify_full_match`]).
//!
//! Ranking uses the cost's mean error only; there is no complexity penalty.

use std::time::Instant;

use neat_core::network::CompiledNetwork;

use crate::cost::{CostKind, accumulate_cost_sum};
use crate::creature_width::validate_observation_width;
use crate::sampling::SampleSpec;

/// Relative tolerance within which a cached final error must equal a direct
/// full-corpus evaluation.
pub const FULL_MATCH_REL_TOLERANCE: f64 = 1e-6;

/// Nested, deterministic record rank in `[0, 1)`.
///
/// The base-2 radical inverse of `index.wrapping_add(seed)`, computed exactly
/// as `(bits.reverse_bits() >> 11) / 2^53`, so it is never `1.0`. The set of
/// records with `rank < r` is nested as `r` grows.
pub fn record_rank(index: u64, seed: u64) -> f64 {
    (index.wrapping_add(seed).reverse_bits() >> 11) as f64 / 9_007_199_254_740_992.0
}

/// A validated successive-halving schedule.
#[derive(Debug, Clone, PartialEq)]
pub struct RungPlan {
    rates: Vec<f64>,
    eta: usize,
}

impl RungPlan {
    /// Validate and build a plan.
    ///
    /// # Errors
    /// When `rates` is empty, any rate is not finite or outside `(0, 1]`, the
    /// rates are not strictly increasing, the last rate is not `1.0`, or
    /// `eta < 2`.
    pub fn new(rates: Vec<f64>, eta: usize) -> Result<Self, String> {
        if rates.is_empty() {
            return Err("rung plan needs at least one rate".to_string());
        }
        for &r in &rates {
            if !r.is_finite() || r <= 0.0 || r > 1.0 {
                return Err(format!("rung rate {r} must be finite and in (0, 1]"));
            }
        }
        if rates.windows(2).any(|w| w[1] <= w[0]) {
            return Err("rung rates must be strictly increasing".to_string());
        }
        if rates[rates.len() - 1] != 1.0 {
            return Err("the last rung rate must be 1.0 (the full corpus)".to_string());
        }
        if eta < 2 {
            return Err(format!("eta must be at least 2, got {eta}"));
        }
        Ok(Self { rates, eta })
    }

    /// The Issue #680 predeclared plan: rates `[0.02, 0.05, 0.15, 1.0]`, eta 2.
    pub fn issue_680_default() -> Self {
        Self {
            rates: vec![0.02, 0.05, 0.15, 1.0],
            eta: 2,
        }
    }

    /// Cumulative sampling rate of each rung.
    pub fn rates(&self) -> &[f64] {
        &self.rates
    }

    /// Keep one in `eta` candidates between rungs.
    pub fn eta(&self) -> usize {
        self.eta
    }
}

/// A packed in-memory corpus of `[inputs..., targets...]` records.
#[derive(Debug, Clone, Copy)]
pub struct PackedCorpus<'a> {
    /// Packed values; the length must be a non-zero whole multiple of
    /// [`PackedCorpus::values_per_record`].
    pub records: &'a [f32],
    /// Input values per record.
    pub num_inputs: usize,
    /// Target values per record.
    pub num_outputs: usize,
    /// Whether the creatures are forward-only (passed to the cost kernels).
    pub forward_only: bool,
}

impl PackedCorpus<'_> {
    /// Values per packed record (`num_inputs + num_outputs`).
    pub fn values_per_record(&self) -> usize {
        self.num_inputs + self.num_outputs
    }

    /// Number of whole records.
    ///
    /// # Errors
    /// When the width is invalid, the length is not a whole multiple of the
    /// record width, or there are no records.
    pub fn record_count(&self) -> Result<usize, String> {
        validate_observation_width(self.num_inputs, self.num_outputs).map_err(|e| e.to_string())?;
        let vpr = self
            .num_inputs
            .checked_add(self.num_outputs)
            .ok_or("record width overflows usize")?;
        if self.records.len() % vpr != 0 {
            return Err(format!(
                "corpus length {} is not a whole multiple of the record width {vpr}",
                self.records.len()
            ));
        }
        if self.records.is_empty() {
            return Err("corpus has zero records".to_string());
        }
        Ok(self.records.len() / vpr)
    }
}

/// For each rung `k`, the global record indices whose rank lies in
/// `[rates[k-1], rates[k])` (with `rates[-1] = 0`). Increments are disjoint,
/// their union is `0..record_count` and each is ascending.
pub fn rung_increments(record_count: usize, plan: &RungPlan, seed: u64) -> Vec<Vec<usize>> {
    let mut out = vec![Vec::new(); plan.rates.len()];
    for i in 0..record_count {
        let rank = record_rank(i as u64, seed);
        // First rung whose cumulative rate exceeds the rank; the last rate is
        // 1.0 and rank < 1.0, so one always exists.
        let k = plan
            .rates
            .iter()
            .position(|&r| rank < r)
            .unwrap_or(plan.rates.len() - 1);
        out[k].push(i);
    }
    out
}

/// Additive sufficient statistics of a decomposable cost.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SufficientStats {
    /// Un-normalised per-record loss sum.
    pub error_sum: f64,
    /// Records contributing to `error_sum`.
    pub record_count: u64,
}

impl SufficientStats {
    /// Mean error under `cost`, or `None` when no records have been seen.
    pub fn mean(&self, cost: CostKind) -> Option<f64> {
        if self.record_count == 0 {
            None
        } else {
            Some(cost.finalise_mean(self.error_sum, self.record_count as usize))
        }
    }
}

/// Which strategy produced an [`ArmOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Arm {
    /// Every candidate on every record.
    Full,
    /// Existing stride sampler with an experimental halving controller.
    IndependentScreening,
    /// Nested strata with cached sufficient statistics.
    CachedHalving,
    /// Same nested subsets as cached halving, re-evaluated from zero.
    NestedUncached,
}

impl Arm {
    /// Stable command/report label for the arm.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::IndependentScreening => "independent-screening",
            Self::CachedHalving => "cached-halving",
            Self::NestedUncached => "nested-uncached",
        }
    }
}

/// Result and cost accounting of one arm.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArmOutcome {
    /// The arm that ran.
    pub arm: Arm,
    /// Winning candidate index.
    pub selected: usize,
    /// Candidate x record forward evaluations actually performed.
    pub record_evaluations: u64,
    /// `SufficientStats` entries held (0 for non-cached arms).
    pub cache_entries: usize,
    /// `cache_entries * size_of::<SufficientStats>()`.
    pub cache_bytes: usize,
    /// Wall-clock seconds updating cached sums and counts (indicative).
    pub cache_update_secs: f64,
    /// Largest temporary packed-record buffer capacity in bytes.
    pub record_buffer_bytes: usize,
    /// Allocated nested record-index capacities in bytes (excluding Vec headers).
    pub record_index_bytes: usize,
    /// Cumulative records available for ranking at each rung, shared by candidates.
    pub rung_records: Vec<usize>,
    /// Strata / subsample construction wall-clock seconds (indicative).
    pub prep_secs: f64,
    /// Scoring wall-clock seconds (indicative).
    pub eval_secs: f64,
    /// Candidates alive entering each rung.
    pub rung_survivors: Vec<usize>,
    /// Per candidate, the rung at which it was rejected; `None` = reached the
    /// final rung.
    pub eliminated_at: Vec<Option<usize>>,
    /// Per candidate, the full-corpus mean error where the arm computed it.
    pub final_errors: Vec<Option<f64>>,
}

/// Sort key: NaN is the worst possible error.
fn rank_key(v: f64) -> f64 {
    if v.is_nan() { f64::INFINITY } else { v }
}

/// Shared elimination: order `alive` by (mean ascending, index ascending, NaN
/// worst), keep `max(1, ceil(alive / eta))`, and record the rejects as
/// eliminated at `rung`. Returns the survivors in ascending index order.
fn eliminate(
    alive: &[usize],
    means: &[f64],
    eta: usize,
    rung: usize,
    eliminated_at: &mut [Option<usize>],
) -> Vec<usize> {
    let mut order: Vec<usize> = alive.to_vec();
    order.sort_by(|&a, &b| {
        rank_key(means[a])
            .total_cmp(&rank_key(means[b]))
            .then(a.cmp(&b))
    });
    let keep = alive.len().div_ceil(eta).max(1);
    for &c in &order[keep..] {
        eliminated_at[c] = Some(rung);
    }
    let mut survivors = order[..keep].to_vec();
    survivors.sort_unstable();
    survivors
}

/// Argmin of mean error over `alive` (NaN worst, ties to the lowest index).
fn argmin(alive: &[usize], means: &[f64]) -> usize {
    alive
        .iter()
        .copied()
        .min_by(|&a, &b| {
            rank_key(means[a])
                .total_cmp(&rank_key(means[b]))
                .then(a.cmp(&b))
        })
        .expect("alive is never empty")
}

fn check_inputs(nets: &[CompiledNetwork], corpus: &PackedCorpus<'_>) -> Result<usize, String> {
    if nets.is_empty() {
        return Err("no candidate networks to score".to_string());
    }
    let n = corpus.record_count()?;
    for (i, net) in nets.iter().enumerate() {
        if net.num_inputs() != corpus.num_inputs {
            return Err(format!(
                "candidate {i} has {} inputs but corpus declares {}",
                net.num_inputs(),
                corpus.num_inputs
            ));
        }
        if corpus.num_outputs > net.num_neurons().saturating_sub(net.num_inputs()) {
            return Err(format!(
                "candidate {i} has fewer non-input neurons than the {} declared outputs",
                corpus.num_outputs
            ));
        }
    }
    if let Some(i) = corpus.records.iter().position(|v| !v.is_finite()) {
        return Err(format!("non-finite corpus value at offset {i}"));
    }
    Ok(n)
}

fn score(
    cost: CostKind,
    net: &mut CompiledNetwork,
    chunk: &[f32],
    corpus: &PackedCorpus<'_>,
) -> Result<f64, String> {
    let sum = accumulate_cost_sum(
        cost,
        net,
        chunk,
        corpus.num_inputs,
        corpus.num_outputs,
        corpus.forward_only,
    )?;
    if !sum.is_finite() {
        return Err(format!("non-finite {} loss sum", cost.as_str()));
    }
    Ok(sum)
}

/// Score every candidate on every record (one rung).
///
/// # Errors
/// When `nets` is empty, the corpus is invalid, or scoring fails.
pub fn run_full(
    cost: CostKind,
    nets: &mut [CompiledNetwork],
    corpus: PackedCorpus<'_>,
) -> Result<ArmOutcome, String> {
    let n = check_inputs(nets, &corpus)?;
    let c = nets.len();
    let started = Instant::now();
    let mut means = vec![0.0; c];
    for (i, net) in nets.iter_mut().enumerate() {
        means[i] = cost.finalise_mean(score(cost, net, corpus.records, &corpus)?, n);
    }
    let alive: Vec<usize> = (0..c).collect();
    Ok(ArmOutcome {
        arm: Arm::Full,
        selected: argmin(&alive, &means),
        record_evaluations: (c * n) as u64,
        cache_entries: 0,
        cache_bytes: 0,
        cache_update_secs: 0.0,
        record_buffer_bytes: 0,
        record_index_bytes: 0,
        rung_records: vec![n],
        prep_secs: 0.0,
        eval_secs: started.elapsed().as_secs_f64(),
        rung_survivors: vec![c],
        eliminated_at: vec![None; c],
        final_errors: means.into_iter().map(Some).collect(),
    })
}

/// Stride-sampler control: rebuild each rung's subsample with the existing
/// [`SampleSpec`] stride (`phase`) and re-score every alive candidate from
/// zero. Rate `1.0` is the full corpus.
///
/// If a rung's subsample is empty (tiny corpora) ranking is impossible, so
/// everyone stays alive for that rung.
///
/// # Errors
/// When `nets` is empty, the corpus is invalid, a rate is rejected by
/// [`SampleSpec::new`], or scoring fails.
pub fn run_independent_screening(
    cost: CostKind,
    nets: &mut [CompiledNetwork],
    corpus: PackedCorpus<'_>,
    plan: &RungPlan,
    phase: u64,
) -> Result<ArmOutcome, String> {
    let n = check_inputs(nets, &corpus)?;
    let c = nets.len();
    let vpr = corpus.values_per_record();
    let mut alive: Vec<usize> = (0..c).collect();
    let mut eliminated_at = vec![None; c];
    let mut final_errors = vec![None; c];
    let mut rung_survivors = Vec::with_capacity(plan.rates.len());
    let (mut evals, mut prep, mut eval) = (0_u64, 0.0, 0.0);
    let last = plan.rates.len() - 1;
    let mut selected = 0;
    let mut rung_records = Vec::new();
    let mut record_buffer_bytes = 0;
    for (k, &rate) in plan.rates.iter().enumerate() {
        rung_survivors.push(alive.len());
        let t = Instant::now();
        let mut buf = corpus.records.to_vec();
        let kept = SampleSpec::new(rate, phase)?
            .sampler()
            .filter_in_place(&mut buf, n, vpr);
        record_buffer_bytes = record_buffer_bytes.max(buf.capacity() * size_of::<f32>());
        rung_records.push(kept);
        prep += t.elapsed().as_secs_f64();
        let mut means = vec![f64::NAN; c];
        if kept > 0 {
            let t = Instant::now();
            for &i in &alive {
                means[i] = cost.finalise_mean(score(cost, &mut nets[i], &buf, &corpus)?, kept);
                evals += kept as u64;
            }
            eval += t.elapsed().as_secs_f64();
        }
        if k == last {
            for &i in &alive {
                final_errors[i] = Some(means[i]);
            }
            selected = argmin(&alive, &means);
        } else if kept > 0 {
            alive = eliminate(&alive, &means, plan.eta, k, &mut eliminated_at);
        }
    }
    Ok(ArmOutcome {
        arm: Arm::IndependentScreening,
        selected,
        record_evaluations: evals,
        cache_entries: 0,
        cache_bytes: 0,
        cache_update_secs: 0.0,
        record_buffer_bytes,
        record_index_bytes: 0,
        rung_records,
        prep_secs: prep,
        eval_secs: eval,
        rung_survivors,
        eliminated_at,
        final_errors,
    })
}

/// The new mechanism: nested strata from [`rung_increments`]; each rung
/// gathers only its new records once (shared by all candidates), adds each
/// alive candidate's sum and count to its [`SufficientStats`] cache and ranks
/// on the cumulative mean.
///
/// # Errors
/// When `nets` is empty, the corpus is invalid, or scoring fails.
pub fn run_cached_halving(
    cost: CostKind,
    nets: &mut [CompiledNetwork],
    corpus: PackedCorpus<'_>,
    plan: &RungPlan,
    seed: u64,
) -> Result<ArmOutcome, String> {
    run_nested(cost, nets, corpus, plan, seed, true)
}

/// Nested uncached control: the same cumulative record IDs and halving policy
/// as [`run_cached_halving`], with every cumulative subset scored from zero.
///
/// # Errors
/// When candidates or corpus are invalid, or scoring yields a non-finite loss.
pub fn run_nested_uncached(
    cost: CostKind,
    nets: &mut [CompiledNetwork],
    corpus: PackedCorpus<'_>,
    plan: &RungPlan,
    seed: u64,
) -> Result<ArmOutcome, String> {
    run_nested(cost, nets, corpus, plan, seed, false)
}

fn run_nested(
    cost: CostKind,
    nets: &mut [CompiledNetwork],
    corpus: PackedCorpus<'_>,
    plan: &RungPlan,
    seed: u64,
    cached: bool,
) -> Result<ArmOutcome, String> {
    let n = check_inputs(nets, &corpus)?;
    let c = nets.len();
    let vpr = corpus.values_per_record();
    let t = Instant::now();
    let increments = rung_increments(n, plan, seed);
    let record_index_bytes = increments
        .iter()
        .map(|v| v.capacity() * size_of::<usize>())
        .sum();
    let mut cache = if cached {
        vec![SufficientStats::default(); c]
    } else {
        Vec::new()
    };
    let mut prep = t.elapsed().as_secs_f64();
    let mut alive: Vec<usize> = (0..c).collect();
    let mut eliminated_at = vec![None; c];
    let mut rung_survivors = Vec::with_capacity(increments.len());
    let mut rung_records = Vec::with_capacity(increments.len());
    let (mut evals, mut eval, mut cache_update_secs) = (0_u64, 0.0, 0.0);
    let mut record_buffer_bytes = 0;
    let last = increments.len() - 1;
    let mut means = vec![f64::NAN; c];
    let mut cumulative = 0;
    for (k, inc) in increments.iter().enumerate() {
        rung_survivors.push(alive.len());
        cumulative += inc.len();
        rung_records.push(cumulative);
        let t = Instant::now();
        let mut buf = Vec::with_capacity(if cached { inc.len() } else { cumulative } * vpr);
        if cached {
            for &i in inc {
                buf.extend_from_slice(&corpus.records[i * vpr..(i + 1) * vpr]);
            }
        } else {
            // Ascending global IDs match an ordinary full sweep on the final rung.
            for (i, record) in corpus.records.chunks_exact(vpr).enumerate() {
                if record_rank(i as u64, seed) < plan.rates[k] {
                    buf.extend_from_slice(record);
                }
            }
        }
        record_buffer_bytes = record_buffer_bytes.max(buf.capacity() * size_of::<f32>());
        prep += t.elapsed().as_secs_f64();
        let count = buf.len() / vpr;
        if count > 0 {
            for &i in &alive {
                let t = Instant::now();
                let sum = score(cost, &mut nets[i], &buf, &corpus)?;
                eval += t.elapsed().as_secs_f64();
                let t = Instant::now();
                if cached {
                    cache[i].error_sum += sum;
                    cache[i].record_count += count as u64;
                    if !cache[i].error_sum.is_finite() {
                        return Err(format!("non-finite cached loss sum for candidate {i}"));
                    }
                    means[i] = cache[i].mean(cost).expect("non-empty increment");
                } else {
                    means[i] = cost.finalise_mean(sum, count);
                }
                if cached {
                    cache_update_secs += t.elapsed().as_secs_f64();
                }
                evals += count as u64;
            }
        }
        // No evidence means no rejection, including an empty initial stratum.
        if k < last && cumulative > 0 {
            alive = eliminate(&alive, &means, plan.eta, k, &mut eliminated_at);
        }
    }
    let mut final_errors = vec![None; c];
    for &i in &alive {
        final_errors[i] = Some(means[i]);
    }
    Ok(ArmOutcome {
        arm: if cached {
            Arm::CachedHalving
        } else {
            Arm::NestedUncached
        },
        selected: argmin(&alive, &means),
        record_evaluations: evals,
        cache_entries: cache.len(),
        cache_bytes: cache.len() * size_of::<SufficientStats>(),
        cache_update_secs,
        record_buffer_bytes,
        record_index_bytes,
        rung_records,
        prep_secs: prep,
        eval_secs: eval,
        rung_survivors,
        eliminated_at,
        final_errors,
    })
}

/// Comparison of an arm's selection against the full-data ground truth.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Audit {
    /// Candidate with the lowest full-corpus error.
    pub full_winner: usize,
    /// Full-corpus error of the arm's selected candidate.
    pub selected_full_error: f64,
    /// Lowest full-corpus error.
    pub best_full_error: f64,
    /// `selected_full_error - best_full_error` (>= 0).
    pub regret: f64,
    /// The full-data winner was rejected at a rung.
    pub lost_full_winner: bool,
    /// Rejected candidates whose full error beats the selected one's.
    pub rejected_better_than_selected: usize,
}

/// Audit `arm` against the Full-arm outcome `full`.
///
/// # Errors
/// When `full` lacks a final error for any candidate, or the candidate counts
/// differ.
pub fn audit(arm: &ArmOutcome, full: &ArmOutcome) -> Result<Audit, String> {
    if full.final_errors.len() != arm.final_errors.len() {
        return Err(format!(
            "candidate count mismatch: arm has {}, full has {}",
            arm.final_errors.len(),
            full.final_errors.len()
        ));
    }
    let errs: Vec<f64> = full
        .final_errors
        .iter()
        .enumerate()
        .map(|(i, e)| e.ok_or_else(|| format!("full outcome has no final error for candidate {i}")))
        .collect::<Result<_, _>>()?;
    if errs.is_empty() || arm.selected >= errs.len() || arm.eliminated_at.len() != errs.len() {
        return Err("invalid audit candidate indices or elimination count".to_string());
    }
    if errs.iter().any(|e| !e.is_finite()) {
        return Err("non-finite authoritative full error".to_string());
    }
    let all: Vec<usize> = (0..errs.len()).collect();
    let full_winner = argmin(&all, &errs);
    let selected_full_error = errs[arm.selected];
    let best_full_error = errs[full_winner];
    let rejected_better_than_selected = arm
        .eliminated_at
        .iter()
        .enumerate()
        .filter(|&(i, e)| e.is_some() && errs[i] < selected_full_error)
        .count();
    Ok(Audit {
        full_winner,
        selected_full_error,
        best_full_error,
        regret: selected_full_error - best_full_error,
        lost_full_winner: arm.eliminated_at[full_winner].is_some(),
        rejected_better_than_selected,
    })
}

/// Maximum relative deviation `|a-b| / max(|b|, 1e-12)` between the arm's
/// final errors and the Full arm's.
///
/// # Errors
/// Naming the candidate, both values and the tolerance when any deviation
/// exceeds [`FULL_MATCH_REL_TOLERANCE`], or when the candidate counts differ.
pub fn verify_full_match(arm: &ArmOutcome, full: &ArmOutcome) -> Result<f64, String> {
    if full.final_errors.len() != arm.final_errors.len() {
        return Err(format!(
            "candidate count mismatch: arm has {}, full has {}",
            arm.final_errors.len(),
            full.final_errors.len()
        ));
    }
    let mut max_dev = 0.0_f64;
    for (i, (a, b)) in arm.final_errors.iter().zip(&full.final_errors).enumerate() {
        let Some(a) = *a else { continue };
        let Some(b) = *b else {
            return Err(format!("full outcome has no final error for candidate {i}"));
        };
        if !a.is_finite() || !b.is_finite() {
            return Err(format!("non-finite final error for candidate {i}"));
        }
        let dev = (a - b).abs() / b.abs().max(1e-12);
        if dev.is_nan() || dev > FULL_MATCH_REL_TOLERANCE {
            return Err(format!(
                "candidate {i} final error {a} deviates from full {b} (relative {dev}) beyond tolerance {FULL_MATCH_REL_TOLERANCE}"
            ));
        }
        max_dev = max_dev.max(dev);
    }
    Ok(max_dev)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::if_tree_fixture::{TreeSpec, corpus_records, tree_creature_json};
    use clap::ValueEnum;
    use neat_core::creature::{compile_creature, parse_creature_json};

    const CANDIDATES: usize = 12;
    const RECORDS: usize = 2048;

    fn nets() -> Vec<CompiledNetwork> {
        (0..CANDIDATES as u64)
            .map(|c| {
                let json = tree_creature_json(&TreeSpec::new(4, 2, 100 + c));
                compile_creature(&parse_creature_json(&json).expect("parse")).expect("compile")
            })
            .collect()
    }

    fn records() -> Vec<f32> {
        corpus_records(&TreeSpec::new(4, 2, 0), RECORDS)
    }

    fn corpus(r: &[f32]) -> PackedCorpus<'_> {
        PackedCorpus {
            records: r,
            num_inputs: 4,
            num_outputs: 1,
            forward_only: true,
        }
    }

    fn count_below(n: u64, rate: f64) -> usize {
        (0..n).filter(|&i| record_rank(i, 0) < rate).count()
    }

    #[test]
    fn rank_is_unit_interval_and_deterministic() {
        for i in 0..5000_u64 {
            let r = record_rank(i, 7);
            assert!((0.0..1.0).contains(&r));
            assert_eq!(r, record_rank(i, 7));
        }
        for rate in [0.02_f64, 0.05, 0.15] {
            assert_eq!(count_below(1024, rate), (rate * 1024.0).ceil() as usize);
        }
    }

    #[test]
    fn strata_are_nested() {
        let set = |r: f64| -> Vec<u64> {
            (0..5000_u64)
                .filter(|&i| record_rank(i, 12345) < r)
                .collect()
        };
        let (a, b, c) = (set(0.02), set(0.05), set(0.15));
        assert!(!a.is_empty() && a.len() < b.len() && b.len() < c.len());
        assert!(a.iter().all(|i| b.contains(i)));
        assert!(b.iter().all(|i| c.contains(i)));
    }

    #[test]
    fn increments_partition_the_corpus() {
        let plan = RungPlan::issue_680_default();
        let n = 5000;
        let inc = rung_increments(n, &plan, 99);
        let mut all: Vec<usize> = inc.iter().flatten().copied().collect();
        all.sort_unstable();
        assert_eq!(all, (0..n).collect::<Vec<_>>());
        let mut cumulative = 0;
        for (k, v) in inc.iter().enumerate() {
            assert!(v.windows(2).all(|w| w[0] < w[1]));
            cumulative += v.len();
            let expected = (0..n as u64)
                .filter(|&i| record_rank(i, 99) < plan.rates()[k])
                .count();
            assert_eq!(cumulative, expected);
        }
    }

    #[test]
    fn plan_refuses_empty() {
        assert_eq!(
            RungPlan::new(vec![], 2).unwrap_err(),
            "rung plan needs at least one rate"
        );
    }

    #[test]
    fn plan_refuses_zero_rate() {
        assert_eq!(
            RungPlan::new(vec![0.0, 1.0], 2).unwrap_err(),
            "rung rate 0 must be finite and in (0, 1]"
        );
    }

    #[test]
    fn plan_refuses_nan_rate() {
        assert_eq!(
            RungPlan::new(vec![f64::NAN, 1.0], 2).unwrap_err(),
            "rung rate NaN must be finite and in (0, 1]"
        );
    }

    #[test]
    fn plan_refuses_rate_above_one() {
        assert_eq!(
            RungPlan::new(vec![0.5, 1.5], 2).unwrap_err(),
            "rung rate 1.5 must be finite and in (0, 1]"
        );
    }

    #[test]
    fn plan_refuses_non_increasing() {
        assert_eq!(
            RungPlan::new(vec![0.1, 0.1, 1.0], 2).unwrap_err(),
            "rung rates must be strictly increasing"
        );
    }

    #[test]
    fn plan_refuses_last_not_one() {
        assert_eq!(
            RungPlan::new(vec![0.1, 0.5], 2).unwrap_err(),
            "the last rung rate must be 1.0 (the full corpus)"
        );
    }

    #[test]
    fn plan_refuses_small_eta() {
        assert_eq!(
            RungPlan::new(vec![0.5, 1.0], 1).unwrap_err(),
            "eta must be at least 2, got 1"
        );
    }

    #[test]
    fn plan_default_is_valid() {
        assert_eq!(
            RungPlan::issue_680_default(),
            RungPlan::new(vec![0.02, 0.05, 0.15, 1.0], 2).unwrap()
        );
    }

    #[test]
    fn every_cost_is_decomposable() {
        let recs = records();
        let c = corpus(&recs);
        let plan = RungPlan::issue_680_default();
        let inc = rung_increments(RECORDS, &plan, 5);
        let mut net = nets().remove(3);
        for &cost in CostKind::value_variants() {
            let whole = score(cost, &mut net, &recs, &c).unwrap();
            assert!(whole.is_finite(), "{cost:?} produced {whole}");
            let mut split = 0.0;
            for v in &inc {
                let mut buf = Vec::new();
                for &i in v {
                    buf.extend_from_slice(&recs[i * 5..(i + 1) * 5]);
                }
                if !buf.is_empty() {
                    split += score(cost, &mut net, &buf, &c).unwrap();
                }
            }
            let dev = (whole - split).abs() / whole.abs().max(1e-12);
            assert!(
                dev <= FULL_MATCH_REL_TOLERANCE,
                "{cost:?}: {whole} vs {split}"
            );
        }
    }

    #[test]
    fn cached_halving_matches_full_for_every_cost() {
        let recs = records();
        let c = corpus(&recs);
        let plan = RungPlan::issue_680_default();
        for &cost in CostKind::value_variants() {
            let mut ns = nets();
            let full = run_full(cost, &mut ns, c).unwrap();
            let cached = run_cached_halving(cost, &mut ns, c, &plan, 11).unwrap();
            verify_full_match(&cached, &full).unwrap_or_else(|e| panic!("{cost:?}: {e}"));
            let sel = cached.selected;
            let (a, b) = (
                cached.final_errors[sel].unwrap(),
                full.final_errors[sel].unwrap(),
            );
            assert!((a - b).abs() / b.abs().max(1e-12) <= FULL_MATCH_REL_TOLERANCE);
        }
    }

    #[test]
    fn evaluation_counts_are_deterministic() {
        let recs = records();
        let c = corpus(&recs);
        let plan = RungPlan::issue_680_default();
        let mut ns = nets();
        let full = run_full(CostKind::Mse, &mut ns, c).unwrap();
        let cached = run_cached_halving(CostKind::Mse, &mut ns, c, &plan, 0).unwrap();
        let indep = run_independent_screening(CostKind::Mse, &mut ns, c, &plan, 0).unwrap();
        assert_eq!(full.record_evaluations, (CANDIDATES * RECORDS) as u64);
        assert_eq!(cached.record_evaluations, 4959);
        assert_eq!(indep.record_evaluations, 6109);
        assert_eq!(cached.rung_survivors, vec![12, 6, 3, 2]);
        assert_eq!(indep.rung_survivors, vec![12, 6, 3, 2]);
        let inc = rung_increments(RECORDS, &plan, 0);
        let want_cached: u64 = (0..4)
            .map(|k| (cached.rung_survivors[k] * inc[k].len()) as u64)
            .sum();
        assert_eq!(cached.record_evaluations, want_cached);
        let want_indep: u64 = (0..4)
            .map(|k| {
                let kept = (RECORDS as f64 * plan.rates()[k]).floor() as usize;
                (indep.rung_survivors[k] * kept) as u64
            })
            .sum();
        assert_eq!(indep.record_evaluations, want_indep);
        assert!(cached.record_evaluations < indep.record_evaluations);
        assert!(indep.record_evaluations < full.record_evaluations);
        assert_eq!(cached.cache_entries, CANDIDATES);
        assert_eq!(
            cached.cache_bytes,
            CANDIDATES * std::mem::size_of::<SufficientStats>()
        );
    }

    #[test]
    fn single_rung_plan_equals_full() {
        let recs = records();
        let c = corpus(&recs);
        let plan = RungPlan::new(vec![1.0], 2).unwrap();
        let mut ns = nets();
        let full = run_full(CostKind::Mse, &mut ns, c).unwrap();
        let cached = run_cached_halving(CostKind::Mse, &mut ns, c, &plan, 3).unwrap();
        assert_eq!(cached.selected, full.selected);
        assert_eq!(cached.record_evaluations, (CANDIDATES * RECORDS) as u64);
    }

    #[test]
    fn empty_early_rungs_do_not_reject_unobserved_candidates() {
        let recs = records();
        let c = corpus(&recs[..5]);
        let plan = RungPlan::issue_680_default();
        let mut ns = nets();
        // rank(0, 1) = 0.5: all three early strata are empty.
        let cached = run_cached_halving(CostKind::Mse, &mut ns, c, &plan, 1).unwrap();
        let full = run_full(CostKind::Mse, &mut ns, c).unwrap();
        assert_eq!(cached.rung_survivors, vec![CANDIDATES; 4]);
        assert_eq!(cached.eliminated_at, vec![None; CANDIDATES]);
        assert_eq!(cached.selected, full.selected);
        assert_eq!(cached.record_evaluations, CANDIDATES as u64);
    }

    #[test]
    fn non_finite_corpus_and_full_reference_are_refused() {
        let mut recs = records();
        let mut ns = nets();
        assert!(run_full(CostKind::Mse, &mut ns, corpus(&recs)).is_ok());
        recs[4] = f32::NAN;
        assert_eq!(
            run_full(CostKind::Mse, &mut ns, corpus(&recs)).unwrap_err(),
            "non-finite corpus value at offset 4"
        );
        let arm = outcome(Arm::Full, 0, vec![None], vec![Some(1.0)]);
        let full = outcome(Arm::Full, 0, vec![None], vec![Some(f64::INFINITY)]);
        assert!(audit(&arm, &full).unwrap_err().contains("non-finite"));
        assert!(
            verify_full_match(&arm, &full)
                .unwrap_err()
                .contains("non-finite")
        );
        assert_eq!(verify_full_match(&arm, &arm).unwrap(), 0.0);
    }

    #[test]
    fn identical_candidates_break_ties_by_index() {
        let recs = records();
        let net = nets().remove(0);
        let mut ns = vec![net; 12];
        let result = run_cached_halving(
            CostKind::Rmse,
            &mut ns,
            corpus(&recs),
            &RungPlan::issue_680_default(),
            0,
        )
        .unwrap();
        assert_eq!(result.selected, 0);
        assert_eq!(
            result.eliminated_at,
            vec![
                None,
                None,
                Some(2),
                Some(1),
                Some(1),
                Some(1),
                Some(0),
                Some(0),
                Some(0),
                Some(0),
                Some(0),
                Some(0)
            ]
        );
    }

    #[test]
    fn rmse_finalises_the_combined_sum_once() {
        let stats = SufficientStats {
            error_sum: 25.0,
            record_count: 2,
        };
        assert_eq!(stats.mean(CostKind::Mse), Some(12.5));
        assert_eq!(stats.mean(CostKind::Rmse), Some(12.5_f64.sqrt()));
        assert_eq!(SufficientStats::default().mean(CostKind::Rmse), None);
    }

    #[test]
    fn uncached_nested_control_uses_the_same_records_and_survivors() {
        let recs = records();
        let c = corpus(&recs);
        let plan = RungPlan::issue_680_default();
        for &cost in CostKind::value_variants() {
            let mut ns = nets();
            let cached = run_cached_halving(cost, &mut ns, c, &plan, 11).unwrap();
            let uncached = run_nested_uncached(cost, &mut ns, c, &plan, 11).unwrap();
            assert_eq!(cached.selected, uncached.selected, "{cost:?}");
            assert_eq!(cached.eliminated_at, uncached.eliminated_at, "{cost:?}");
            let increments = rung_increments(RECORDS, &plan, 11);
            let mut cumulative = 0;
            let mut expected = 0;
            for (k, inc) in increments.iter().enumerate() {
                cumulative += inc.len();
                expected += (cumulative * uncached.rung_survivors[k]) as u64;
            }
            assert_eq!(uncached.record_evaluations, expected);
            assert_eq!(uncached.cache_bytes, 0);
            assert!(cached.record_evaluations < uncached.record_evaluations);
            verify_full_match(&cached, &uncached).unwrap();
        }
    }

    #[test]
    fn invalid_inputs_are_refused() {
        let recs = records();
        let plan = RungPlan::issue_680_default();
        let mut none: Vec<CompiledNetwork> = Vec::new();
        assert_eq!(
            run_full(CostKind::Mse, &mut none, corpus(&recs)).unwrap_err(),
            "no candidate networks to score"
        );
        let mut ns = nets();
        let err =
            run_cached_halving(CostKind::Mse, &mut ns, corpus(&recs[..7]), &plan, 0).unwrap_err();
        assert_eq!(
            err,
            "corpus length 7 is not a whole multiple of the record width 5"
        );
        let err =
            run_independent_screening(CostKind::Mse, &mut ns, corpus(&[]), &plan, 0).unwrap_err();
        assert_eq!(err, "corpus has zero records");
    }

    #[test]
    fn candidate_shape_is_checked_before_entering_loss_kernels() {
        let recs = records();
        let ns = nets();
        assert_eq!(check_inputs(&ns, &corpus(&recs)).unwrap(), RECORDS);
        let wrong_inputs = PackedCorpus {
            num_inputs: 3,
            num_outputs: 2,
            ..corpus(&recs)
        };
        assert_eq!(
            check_inputs(&ns, &wrong_inputs).unwrap_err(),
            "candidate 0 has 4 inputs but corpus declares 3"
        );
        let wide_record = vec![0.0_f32; 1004];
        let wrong_outputs = PackedCorpus {
            num_outputs: 1000,
            ..corpus(&wide_record)
        };
        assert_eq!(
            check_inputs(&ns, &wrong_outputs).unwrap_err(),
            "candidate 0 has fewer non-input neurons than the 1000 declared outputs"
        );
    }

    fn outcome(
        arm: Arm,
        selected: usize,
        elim: Vec<Option<usize>>,
        finals: Vec<Option<f64>>,
    ) -> ArmOutcome {
        ArmOutcome {
            arm,
            selected,
            record_evaluations: 0,
            cache_entries: 0,
            cache_bytes: 0,
            cache_update_secs: 0.0,
            record_buffer_bytes: 0,
            record_index_bytes: 0,
            rung_records: vec![],
            prep_secs: 0.0,
            eval_secs: 0.0,
            rung_survivors: vec![],
            eliminated_at: elim,
            final_errors: finals,
        }
    }

    #[test]
    fn audit_reports_regret_and_lost_winner() {
        let full = outcome(
            Arm::Full,
            2,
            vec![None; 4],
            vec![Some(0.5), Some(0.4), Some(0.1), Some(0.3)],
        );
        // Winner 2 rejected at rung 0; candidate 3 selected; 1 also rejected.
        let arm = outcome(
            Arm::CachedHalving,
            3,
            vec![Some(1), Some(1), Some(0), None],
            vec![None, None, None, Some(0.3)],
        );
        let a = audit(&arm, &full).unwrap();
        assert_eq!(a.full_winner, 2);
        assert!(a.lost_full_winner);
        assert_eq!(a.selected_full_error, 0.3);
        assert_eq!(a.best_full_error, 0.1);
        assert!((a.regret - 0.2).abs() < 1e-15);
        assert_eq!(a.rejected_better_than_selected, 1);

        let ok = outcome(
            Arm::CachedHalving,
            2,
            vec![Some(0), Some(0), None, Some(1)],
            vec![None, None, Some(0.1), None],
        );
        let a = audit(&ok, &full).unwrap();
        assert!(!a.lost_full_winner);
        assert_eq!(a.regret, 0.0);
        assert_eq!(a.rejected_better_than_selected, 0);
    }

    #[test]
    fn verify_full_match_fails_loudly() {
        let full = outcome(Arm::Full, 0, vec![None; 2], vec![Some(1.0), Some(2.0)]);
        let bad = outcome(
            Arm::CachedHalving,
            1,
            vec![Some(0), None],
            vec![None, Some(2.0 * (1.0 + 1e-3))],
        );
        let err = verify_full_match(&bad, &full).unwrap_err();
        assert!(err.starts_with("candidate 1 final error"), "{err}");
        assert!(err.contains("tolerance 0.000001"), "{err}");
        let good = outcome(
            Arm::CachedHalving,
            1,
            vec![Some(0), None],
            vec![None, Some(2.0)],
        );
        assert_eq!(verify_full_match(&good, &full).unwrap(), 0.0);
    }
}
