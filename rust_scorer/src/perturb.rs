//! Perturbation-robustness mode — Issue #588.
//!
//! Every optimiser in the family accepts on a point estimate of a sharp
//! objective, and its wins are tiny (≈ 6e-05). At that scale "scores higher"
//! and "is better" are different claims: a win on a knife-edge optimum, lost
//! to a 1e-3 jostle of the weights, is not the asset a win that survives it is.
//! Evolution Strategies (Salimans et al. 2017) optimise the **smoothed**
//! objective `E[f(θ + σε)]`; this mode measures it.
//!
//! `--perturb-sigma σ --perturb-seeds n` scores each creature as usual — the
//! unperturbed result is **unchanged**, computed on exactly the path it would be
//! without the flag — and additionally scores `n` copies whose synapse weights
//! are perturbed by `N(0, σ²)`, reporting the mean and standard deviation of
//! their error and score in a `perturbation` block.
//!
//! ## Common random numbers
//!
//! Copy `k`'s noise on a synapse is a pure function of `(seed_base + k, from
//! UUID, to UUID, role)`: every creature in a run is perturbed with the same
//! seeds, and two creatures that share a synapse receive the **same** noise on
//! it, whatever order their synapses are listed in. Comparisons between
//! candidates are therefore paired, the other half of the ES paper's
//! discipline, so a difference in smoothed score is not drowned in the noise of
//! independent draws.
//!
//! All `n × creatures` copies are scored in one extra CPU directory sweep over
//! in-memory creatures; nothing is written to disk. Only synapse weights are
//! perturbed (biases are left alone), so σ is calibrated against the weight
//! distribution, which each summary reports (`weightRms`, `weightMedianAbs`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use neat_core::creature::{CreatureExport, SynapseExport};
use serde::Serialize;

use crate::cost::CostKind;
use crate::gpu::GpuBackendLabel;
use crate::multi_score::{LoadedCreature, score_loaded_creatures_cpu};
use crate::sampling::SampleSpec;
use crate::scoring::ScoreResult;

/// Largest accepted `--perturb-seeds`: each seed is a full extra scoring of
/// every creature.
pub const MAX_PERTURB_SEEDS: usize = 1024;

/// A validated perturbation request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PerturbSpec {
    sigma: f64,
    seeds: usize,
    seed_base: u64,
}

impl PerturbSpec {
    /// `sigma` must be finite and `> 0`; `seeds` in `1..=MAX_PERTURB_SEEDS`.
    pub fn new(sigma: f64, seeds: usize, seed_base: u64) -> Result<Self, String> {
        if !sigma.is_finite() || sigma <= 0.0 {
            return Err(format!(
                "--perturb-sigma must be a finite value > 0, got {sigma}"
            ));
        }
        if seeds == 0 || seeds > MAX_PERTURB_SEEDS {
            return Err(format!(
                "--perturb-seeds must be in 1..={MAX_PERTURB_SEEDS}, got {seeds}"
            ));
        }
        Ok(Self {
            sigma,
            seeds,
            seed_base,
        })
    }

    /// Standard deviation of the weight noise.
    #[must_use]
    pub const fn sigma(&self) -> f64 {
        self.sigma
    }

    /// Number of perturbed copies per creature.
    #[must_use]
    pub const fn seeds(&self) -> usize {
        self.seeds
    }
}

/// The `perturbation` block attached to a creature's result.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerturbationSummary {
    /// Noise standard deviation.
    pub sigma: f64,
    /// Number of perturbed copies scored.
    pub seeds: usize,
    /// First seed; copy `k` uses `seedBase + k`.
    pub seed_base: u64,
    /// What was perturbed (`"synapseWeights"`).
    pub target: &'static str,
    /// Mean error over the perturbed copies.
    pub mean_error: f64,
    /// Sample standard deviation of the perturbed errors (`0` for one seed).
    pub std_error: f64,
    /// Mean score over the perturbed copies.
    pub mean_score: f64,
    /// Sample standard deviation of the perturbed scores.
    pub std_score: f64,
    /// Perturbed errors in seed order.
    pub errors: Vec<f64>,
    /// Perturbed scores in seed order.
    pub scores: Vec<f64>,
    /// Root mean square of the unperturbed synapse weights (σ calibration).
    pub weight_rms: f64,
    /// Median absolute unperturbed synapse weight (σ calibration).
    pub weight_median_abs: f64,
}

const fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn fnv1a(bytes: &[u8], mut h: u64) -> u64 {
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Standard-normal noise for `synapse` under `seed` — a pure function of the
/// seed and the synapse's `(from, to, role)` identity (Box–Muller over two
/// SplitMix64 draws).
#[must_use]
pub fn synapse_noise(seed: u64, synapse: &SynapseExport) -> f64 {
    let mut h = fnv1a(synapse.from_uuid.as_bytes(), 0xCBF2_9CE4_8422_2325);
    h = fnv1a(&[0], h);
    h = fnv1a(synapse.to_uuid.as_bytes(), h);
    h = fnv1a(&[0], h);
    h = fnv1a(synapse.synapse_type.as_deref().unwrap_or("").as_bytes(), h);
    let a = mix(h ^ mix(seed));
    let b = mix(a);
    // u1 in (0, 1] so ln(u1) is finite; u2 in [0, 1).
    let u1 = ((a >> 11) + 1) as f64 / (1_u64 << 53) as f64;
    let u2 = (b >> 11) as f64 / (1_u64 << 53) as f64;
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

/// `creature` with every synapse weight moved by `sigma * synapse_noise(seed)`.
#[must_use]
pub fn perturbed(creature: &CreatureExport, sigma: f64, seed: u64) -> CreatureExport {
    let mut out = creature.clone();
    for s in &mut out.synapses {
        s.weight += sigma * synapse_noise(seed, s);
    }
    out
}

/// `(rms, median |w|)` of the creature's synapse weights (`(0, 0)` for none).
#[must_use]
pub fn weight_stats(creature: &CreatureExport) -> (f64, f64) {
    let n = creature.synapses.len();
    if n == 0 {
        return (0.0, 0.0);
    }
    let rms = (creature
        .synapses
        .iter()
        .map(|s| s.weight * s.weight)
        .sum::<f64>()
        / n as f64)
        .sqrt();
    let mut abs: Vec<f64> = creature.synapses.iter().map(|s| s.weight.abs()).collect();
    abs.sort_by(f64::total_cmp);
    let median = if n % 2 == 1 {
        abs[n / 2]
    } else {
        (abs[n / 2 - 1] + abs[n / 2]) / 2.0
    };
    (rms, median)
}

fn mean_std(values: &[f64]) -> (f64, f64) {
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    if values.len() < 2 {
        return (mean, 0.0);
    }
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0);
    (mean, var.sqrt())
}

/// One creature to perturb: its result key and parsed export.
pub struct PerturbTarget {
    /// Result-map key (file stem).
    pub key: String,
    /// Path, for diagnostics.
    pub path: PathBuf,
    /// The unperturbed creature.
    pub creature: CreatureExport,
}

/// Score `spec.seeds()` perturbed copies of every target in one CPU directory
/// sweep and summarise them per target key.
pub fn score_perturbations(
    targets: &[PerturbTarget],
    data_path: &Path,
    cost: CostKind,
    sample: &SampleSpec,
    spec: &PerturbSpec,
) -> Result<BTreeMap<String, PerturbationSummary>, String> {
    let started = Instant::now();
    let mut copies = Vec::with_capacity(targets.len() * spec.seeds);
    for (t, target) in targets.iter().enumerate() {
        for k in 0..spec.seeds {
            copies.push(LoadedCreature {
                // Internal keys only: never printed.
                key: format!("{t:08}-{k:04}"),
                path: target.path.clone(),
                creature: perturbed(
                    &target.creature,
                    spec.sigma,
                    spec.seed_base.wrapping_add(k as u64),
                ),
            });
        }
    }
    let scored = score_loaded_creatures_cpu(
        copies,
        data_path,
        GpuBackendLabel::CpuFallback,
        cost,
        None,
        sample,
        started,
    )?;
    let mut out = BTreeMap::new();
    for (t, target) in targets.iter().enumerate() {
        let results: Vec<&ScoreResult> = (0..spec.seeds)
            .map(|k| &scored[&format!("{t:08}-{k:04}")])
            .collect();
        let errors: Vec<f64> = results.iter().map(|r| r.error).collect();
        let scores: Vec<f64> = results.iter().map(|r| r.score).collect();
        let (mean_error, std_error) = mean_std(&errors);
        let (mean_score, std_score) = mean_std(&scores);
        let (weight_rms, weight_median_abs) = weight_stats(&target.creature);
        out.insert(
            target.key.clone(),
            PerturbationSummary {
                sigma: spec.sigma,
                seeds: spec.seeds,
                seed_base: spec.seed_base,
                target: "synapseWeights",
                mean_error,
                std_error,
                mean_score,
                std_score,
                errors,
                scores,
                weight_rms,
                weight_median_abs,
            },
        );
    }
    Ok(out)
}

/// Every creature in `creatures_dir`, as directory mode loads them.
pub fn targets_from_dir(creatures_dir: &Path) -> Result<Vec<PerturbTarget>, String> {
    Ok(crate::multi_score::load_creatures_from_dir(creatures_dir)?
        .into_iter()
        .map(|c| PerturbTarget {
            key: c.key,
            path: c.path,
            creature: c.creature,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn syn(from: &str, to: &str, ty: Option<&str>) -> SynapseExport {
        SynapseExport {
            from_uuid: from.to_string(),
            to_uuid: to.to_string(),
            weight: 0.5,
            synapse_type: ty.map(str::to_string),
        }
    }

    #[test]
    fn noise_is_a_pure_function_of_seed_and_synapse_identity() {
        let a = syn("input-0", "h1", None);
        assert_eq!(
            synapse_noise(3, &a).to_bits(),
            synapse_noise(3, &a.clone()).to_bits()
        );
        assert_ne!(synapse_noise(3, &a), synapse_noise(4, &a));
        assert_ne!(
            synapse_noise(3, &a),
            synapse_noise(3, &syn("input-1", "h1", None))
        );
        assert_ne!(
            synapse_noise(3, &a),
            synapse_noise(3, &syn("input-0", "h1", Some("condition")))
        );
        // Weight is not part of the identity: a changed weight gets the same noise.
        let mut moved = a.clone();
        moved.weight = 9.0;
        assert_eq!(
            synapse_noise(3, &a).to_bits(),
            synapse_noise(3, &moved).to_bits()
        );
    }

    #[test]
    fn noise_is_standard_normal() {
        let draws: Vec<f64> = (0..20_000)
            .map(|i| synapse_noise(11, &syn(&format!("input-{i}"), "out", None)))
            .collect();
        let (mean, std) = mean_std(&draws);
        assert!(mean.abs() < 0.03, "mean {mean}");
        assert!((std - 1.0).abs() < 0.03, "std {std}");
        assert!(draws.iter().all(|d| d.is_finite()));
    }

    #[test]
    fn spec_rejects_bad_values() {
        assert!(PerturbSpec::new(1e-3, 8, 0).is_ok());
        for sigma in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(PerturbSpec::new(sigma, 8, 0).is_err(), "{sigma}");
        }
        assert!(PerturbSpec::new(1e-3, 0, 0).is_err());
        assert!(PerturbSpec::new(1e-3, MAX_PERTURB_SEEDS + 1, 0).is_err());
    }

    #[test]
    fn mean_std_uses_the_sample_estimator() {
        assert_eq!(mean_std(&[2.0]), (2.0, 0.0));
        let (m, s) = mean_std(&[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(m, 2.5);
        assert!((s - (5.0_f64 / 3.0).sqrt()).abs() < 1e-15);
    }
}
