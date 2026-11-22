//! Ensemble mode — Issue #589.
//!
//! The fleet evolves a population and crowns one champion; the rest is
//! discarded. Averaging the members' predictions is never worse than the
//! average member under squared error — the Krogh & Vedelsby (1995) ambiguity
//! decomposition:
//!
//! ```text
//! E_ensemble = Ē − Ā,   Ā ≥ 0
//! ```
//!
//! where `Ē` is the mean of the members' own errors and `Ā` the mean ambiguity
//! (how far members spread around the ensemble prediction). It guarantees
//! beating the **mean** member, not the **best** one; whether the ensemble beats
//! the champion depends on `Ā`, which this mode measures.
//!
//! [`score_from_creature_dir_ensemble`] scores a creatures directory in the
//! ordinary single corpus pass and, from the same per-record outputs, reports
//! an [`EnsembleReport`] for the uniform mean prediction of the members (all of
//! them, or the `top_k` with the lowest own error).
//!
//! ## Per-creature results are directory mode's, bit for bit
//!
//! Each creature's outputs come from `neat-core`'s public batch forward pass
//! (`CompiledNetwork::score_records_flat_into`), driven so every record lands in
//! the same SIMD tier the fused MSE scan gives it in a worker range (8-record
//! groups; a 4-record group run as a padded 8-group, whose lanes are
//! independent; the `n % 4` tail one record at a time), over directory mode's
//! own worker partition. The squared error is folded in record order, so the
//! per-creature map is the one plain directory mode prints, bit for bit.
//!
//! ## Top-k in one pass
//!
//! Membership by own full-corpus error is only known after the pass, so the
//! pass keeps, per output `j`, the sufficient statistics of squared error for
//! **every** subset: `Σ t²`, `Σ t·oₐ` per member and the Gram matrix
//! `Σ oₐ·o_b`. For a subset `S` of size `K`:
//!
//! ```text
//! SSE_j(S) = Σ t² − (2/K)·Σ_{a∈S} Σ t·oₐ + (1/K²)·Σ_{a,b∈S} Σ oₐ·o_b
//! ```
//!
//! `ensembleError` is `Σ_j SSE_j / (records · outputs)`, and `meanMemberError`
//! / `bestMemberError` are the same expression for one-member subsets, so all
//! three terms share one arithmetic. (They agree with each creature's printed
//! `error` — which takes the `f32` difference `t − o` first, like directory
//! mode — to `f32` rounding.) `ambiguity` is **defined** as
//! `meanMemberError − ensembleError`, the identity, so a negative value beyond
//! `f64` rounding is a bug signal, not noise to estimate around.
//!
//! The mode never emits a population candidate and carries no `score`: an
//! ensemble has no single complexity. `MSE` only, forward-only members only.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use neat_core::CompiledNetwork;
use neat_core::creature::compile_creature;
use neat_core::training_bin_stream::for_each_read_chunk;
use neat_core::training_data::{TrainingDataConfig, find_bin_files};
use rayon::prelude::*;
use serde::Serialize;

use crate::cost::CostKind;
use crate::gpu::GpuBackendLabel;
use crate::multi_score::{
    self, DirectoryRunMeta, finalise_directory_results, load_creatures_from_dir,
    partition_packed_record_ranges, worker_split_factor, workers_per_creature_split,
};
use crate::sampling::SampleSpec;
use crate::scoring::ScoreResult;

/// Result-map key the ensemble block is printed under.
pub const ENSEMBLE_KEY: &str = "ensemble";

/// How members' predictions are combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum EnsembleMethod {
    /// Uniform element-wise mean of the members' outputs.
    Mean,
}

/// The ensemble block: all three terms of the decomposition plus the champion.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnsembleReport {
    /// Combination method (`"mean"`).
    pub method: &'static str,
    /// Number of members averaged.
    pub members: usize,
    /// Members' file stems, lowest own error first.
    pub member_keys: Vec<String>,
    /// `Ē` — mean of the members' own errors (from the pass's f64 statistics;
    /// within `f32` rounding of each member's per-creature `error`).
    pub mean_member_error: f64,
    /// Error of the mean prediction.
    pub ensemble_error: f64,
    /// `Ā = Ē − E_ensemble` (the identity, never estimated separately).
    pub ambiguity: f64,
    /// Lowest own error among the members (same statistics as `Ē`).
    pub best_member_error: f64,
    /// True when the ensemble's error is strictly below the best member's.
    pub beats_best_member: bool,
}

/// Sufficient statistics of squared error for every member subset.
#[derive(Debug, Clone)]
pub struct EnsembleStats {
    members: usize,
    outputs: usize,
    records: usize,
    /// `target_sq[j] = Σ t_j²`.
    target_sq: Vec<f64>,
    /// `cross[j * members + a] = Σ t_j · o_{a,j}`.
    cross: Vec<f64>,
    /// `gram[(j * members + a) * members + b] = Σ o_{a,j} · o_{b,j}`.
    gram: Vec<f64>,
}

impl EnsembleStats {
    /// Empty statistics for `members` members with `outputs` outputs.
    #[must_use]
    pub fn new(members: usize, outputs: usize) -> Self {
        Self {
            members,
            outputs,
            records: 0,
            target_sq: vec![0.0; outputs],
            cross: vec![0.0; outputs * members],
            gram: vec![0.0; outputs * members * members],
        }
    }

    /// Fold one record: `targets[j]` and `output(a, j)` for member `a`.
    pub fn add_record(&mut self, targets: &[f32], output: impl Fn(usize, usize) -> f32) {
        let m = self.members;
        let mut o = vec![0.0_f64; m];
        for (j, &t) in targets.iter().enumerate().take(self.outputs) {
            let t = f64::from(t);
            self.target_sq[j] += t * t;
            for (a, slot) in o.iter_mut().enumerate() {
                *slot = f64::from(output(a, j));
            }
            let cross = &mut self.cross[j * m..(j + 1) * m];
            for (c, &oa) in cross.iter_mut().zip(&o) {
                *c += t * oa;
            }
            let gram = &mut self.gram[j * m * m..(j + 1) * m * m];
            for (a, &oa) in o.iter().enumerate() {
                let row = &mut gram[a * m..(a + 1) * m];
                for (g, &ob) in row.iter_mut().zip(&o) {
                    *g += oa * ob;
                }
            }
        }
        self.records += 1;
    }

    /// Merge statistics gathered over a disjoint set of records.
    pub fn merge(&mut self, other: &Self) {
        assert_eq!((self.members, self.outputs), (other.members, other.outputs));
        self.records += other.records;
        for (a, b) in self.target_sq.iter_mut().zip(&other.target_sq) {
            *a += b;
        }
        for (a, b) in self.cross.iter_mut().zip(&other.cross) {
            *a += b;
        }
        for (a, b) in self.gram.iter_mut().zip(&other.gram) {
            *a += b;
        }
    }

    /// Mean squared error of the uniform mean prediction of `subset`.
    #[must_use]
    pub fn ensemble_error(&self, subset: &[usize]) -> f64 {
        let m = self.members;
        let k = subset.len() as f64;
        let mut sse = 0.0;
        for j in 0..self.outputs {
            let cross: f64 = subset.iter().map(|&a| self.cross[j * m + a]).sum();
            let mut gram = 0.0;
            for &a in subset {
                for &b in subset {
                    gram += self.gram[(j * m + a) * m + b];
                }
            }
            sse += self.target_sq[j] - 2.0 * cross / k + gram / (k * k);
        }
        sse / (self.records as f64 * self.outputs as f64)
    }

    /// The report for the `top_k` members with the lowest `member_errors`
    /// (all members when `None`). Ties rank by key.
    pub fn report(
        &self,
        member_errors: &[(String, f64)],
        top_k: Option<usize>,
    ) -> Result<EnsembleReport, String> {
        assert_eq!(member_errors.len(), self.members);
        let k = top_k.unwrap_or(self.members);
        if k == 0 || k > self.members {
            return Err(format!(
                "--ensemble-top-k {k} is outside 1..={} (the directory's member count)",
                self.members
            ));
        }
        let mut order: Vec<usize> = (0..self.members).collect();
        order.sort_by(|&a, &b| {
            member_errors[a]
                .1
                .total_cmp(&member_errors[b].1)
                .then_with(|| member_errors[a].0.cmp(&member_errors[b].0))
        });
        order.truncate(k);
        // Every decomposition term comes from the same f64 statistics, so the
        // identity is exact up to f64 rounding: a one-member ensemble has zero
        // ambiguity, not the ~1e-10 an f32-difference member error would leave.
        let own: Vec<f64> = order.iter().map(|&a| self.ensemble_error(&[a])).collect();
        let mean_member_error = own.iter().sum::<f64>() / k as f64;
        let best_member_error = own.iter().copied().fold(f64::INFINITY, f64::min);
        let ensemble_error = self.ensemble_error(&order);
        Ok(EnsembleReport {
            method: "mean",
            members: k,
            member_keys: order.iter().map(|&a| member_errors[a].0.clone()).collect(),
            mean_member_error,
            ensemble_error,
            ambiguity: mean_member_error - ensemble_error,
            best_member_error,
            beats_best_member: ensemble_error < best_member_error,
        })
    }
}

/// Outputs of `records` (packed `[inputs…, targets…]`) into `out`, each record
/// in the SIMD tier the fused MSE scan would score it in for a worker range of
/// this length, and the range's summed per-record MSE in record order.
fn outputs_and_mse_sum(
    net: &CompiledNetwork,
    records: &[f32],
    num_inputs: usize,
    num_outputs: usize,
    out: &mut [f32],
) -> f64 {
    let vpr = num_inputs + num_outputs;
    let n = records.len() / vpr;
    debug_assert_eq!(out.len(), n * num_outputs);
    let g8 = n - n % 8;
    if g8 > 0 {
        net.score_records_flat_into(
            &records[..g8 * vpr],
            vpr,
            num_outputs,
            &mut out[..g8 * num_outputs],
        );
    }
    let mut tail = g8;
    if n - g8 >= 4 {
        // A 4-record group, run as an 8-group padded with copies: lanes are
        // independent, so the first four lanes are the 4-group's results.
        let quad = &records[g8 * vpr..(g8 + 4) * vpr];
        let padded: Vec<f32> = quad.iter().chain(quad).copied().collect();
        let mut padded_out = vec![0.0_f32; 8 * num_outputs];
        net.score_records_flat_into(&padded, vpr, num_outputs, &mut padded_out);
        out[g8 * num_outputs..(g8 + 4) * num_outputs]
            .copy_from_slice(&padded_out[..4 * num_outputs]);
        tail += 4;
    }
    if tail < n {
        // < 4 records: the single-record kernel.
        net.score_records_flat_into(
            &records[tail * vpr..],
            vpr,
            num_outputs,
            &mut out[tail * num_outputs..],
        );
    }
    let inv_outputs = 1.0 / (num_outputs as f64);
    let mut sum = 0.0_f64;
    for r in 0..n {
        let target = &records[r * vpr + num_inputs..(r + 1) * vpr];
        let mut sq_sum = 0.0_f64;
        for (t, o) in target
            .iter()
            .zip(&out[r * num_outputs..(r + 1) * num_outputs])
        {
            let diff = (*t - *o) as f64;
            sq_sum += diff * diff;
        }
        sum += sq_sum * inv_outputs;
    }
    sum
}

/// Score `creatures_dir` in one corpus pass and report the mean ensemble of
/// its members (Issue #589). Returns the ordinary per-creature map (bit-identical
/// to plain directory mode) and the ensemble block.
pub fn score_from_creature_dir_ensemble(
    creatures_dir: &Path,
    data_path: &Path,
    cost: CostKind,
    sample: &SampleSpec,
    top_k: Option<usize>,
) -> Result<(BTreeMap<String, ScoreResult>, EnsembleReport), String> {
    let started = Instant::now();
    if cost != CostKind::Mse {
        return Err(format!(
            "--ensemble needs --cost MSE (the ambiguity decomposition is a squared-error identity); got {}",
            cost.as_str()
        ));
    }
    let loaded = load_creatures_from_dir(creatures_dir)?;
    if let Some(c) = loaded.iter().find(|c| !c.creature.forward_only) {
        return Err(format!(
            "--ensemble scores forward-only creatures only; '{}' is recurrent (forwardOnly=false)",
            c.path.display()
        ));
    }
    if let Some(c) = loaded.iter().find(|c| c.key == ENSEMBLE_KEY) {
        return Err(format!(
            "creature '{}' would collide with the '{ENSEMBLE_KEY}' result key",
            c.path.display()
        ));
    }
    if let Some(k) = top_k
        && (k == 0 || k > loaded.len())
    {
        return Err(format!(
            "--ensemble-top-k {k} is outside 1..={} (the directory's member count)",
            loaded.len()
        ));
    }
    let num_inputs = loaded[0].creature.input;
    let num_outputs = loaded[0].creature.output;
    let n_creatures = loaded.len();

    let compile_started = Instant::now();
    let mut networks = Vec::with_capacity(n_creatures);
    for c in &loaded {
        multi_score::compile_probe::record_compile();
        networks.push(compile_creature(&c.creature).map_err(|e| {
            format!(
                "Failed compiling worker network for creature '{}': {e}",
                c.path.display()
            )
        })?);
    }
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
    let record_bytes = TrainingDataConfig {
        num_inputs,
        num_outputs,
    }
    .bytes_per_record();
    crate::corpus_guard::assert_records_aligned(&bin_files, record_bytes)?;
    let vpr = num_inputs + num_outputs;
    let read_target = crate::read_tuning::training_read_target_bytes_from_env(record_bytes);
    let read_buf_len = crate::stream_score::effective_fused_read_buf_len(record_bytes, read_target);
    let activation_threads = crate::stream_score::activation_worker_count_for_scorer();
    // Directory mode's own partition, so each creature's sums match it exactly.
    let workers_per = workers_per_creature_split(
        n_creatures,
        activation_threads,
        worker_split_factor(),
        crate::host_resources::max_worker_count(&crate::host_resources::host()),
    );

    let mut total_loss = vec![0.0_f64; n_creatures];
    let mut total_records = 0_usize;
    let mut stats = EnsembleStats::new(n_creatures, num_outputs);
    // outputs[c] holds creature c's outputs for the current chunk.
    let mut outputs: Vec<Vec<f32>> = vec![Vec::new(); n_creatures];

    let mut score_chunk = |floats: &mut Vec<f32>, n_records: usize| -> Result<(), String> {
        let floats: &[f32] = floats;
        if floats.len() != n_records * vpr {
            return Err("Internal float unpack length mismatch".to_string());
        }
        total_records += n_records;
        // Per creature: its worker ranges (directory mode's partition).
        let mut tasks: Vec<(usize, std::ops::Range<usize>)> = Vec::new();
        for (c, &w) in workers_per.iter().enumerate() {
            let actual = w.min(n_records).max(1);
            if actual == 1 {
                tasks.push((c, 0..floats.len()));
            } else {
                for r in partition_packed_record_ranges(vpr, n_records, actual) {
                    tasks.push((c, r));
                }
            }
        }
        let sums: Vec<(usize, std::ops::Range<usize>, Vec<f32>, f64)> = tasks
            .into_par_iter()
            .map(|(c, range)| {
                let n = range.len() / vpr;
                let mut out = vec![0.0_f32; n * num_outputs];
                let s = outputs_and_mse_sum(
                    &networks[c],
                    &floats[range.clone()],
                    num_inputs,
                    num_outputs,
                    &mut out,
                );
                (c, range, out, s)
            })
            .collect();
        for o in outputs.iter_mut() {
            o.clear();
            o.resize(n_records * num_outputs, 0.0);
        }
        // Fold in worker order, as directory mode does.
        for (c, range, out, s) in sums {
            total_loss[c] += s;
            let first = range.start / vpr * num_outputs;
            outputs[c][first..first + out.len()].copy_from_slice(&out);
        }
        // Ensemble statistics: records split across threads, merged in order.
        let threads = activation_threads.max(1).min(n_records);
        let per = n_records.div_ceil(threads);
        let partials: Vec<EnsembleStats> = (0..threads)
            .into_par_iter()
            .map(|t| {
                let mut part = EnsembleStats::new(n_creatures, num_outputs);
                for r in t * per..((t + 1) * per).min(n_records) {
                    let targets = &floats[r * vpr + num_inputs..(r + 1) * vpr];
                    part.add_record(targets, |a, j| outputs[a][r * num_outputs + j]);
                }
                part
            })
            .collect();
        for p in &partials {
            stats.merge(p);
        }
        Ok(())
    };

    multi_score::training_pass_probe::record_sweep();
    let mut pending: Vec<u8> = Vec::new();
    let mut head = 0_usize;
    let mut unpack_floats: Vec<f32> = Vec::new();
    let mut sampler = sample.sampler();
    for_each_read_chunk(&bin_files, read_buf_len, |chunk| {
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
            gpu_backend: GpuBackendLabel::CpuFallback,
            read_buf_len,
            activation_threads,
            time_taken_secs: started.elapsed().as_secs_f64(),
            compile_time_secs,
            sample,
        },
    )?;
    let member_errors: Vec<(String, f64)> = loaded
        .iter()
        .map(|c| (c.key.clone(), results[&c.key].error))
        .collect();
    let report = stats.report(&member_errors, top_k)?;
    Ok((results, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats_for(outputs: &[Vec<f32>], targets: &[f32]) -> EnsembleStats {
        let mut s = EnsembleStats::new(outputs.len(), 1);
        for (r, &t) in targets.iter().enumerate() {
            s.add_record(&[t], |a, _| outputs[a][r]);
        }
        s
    }

    fn mse(o: &[f32], t: &[f32]) -> f64 {
        o.iter()
            .zip(t)
            .map(|(o, t)| f64::from(t - o).powi(2))
            .sum::<f64>()
            / t.len() as f64
    }

    #[test]
    fn hand_computed_decomposition_holds() {
        // Two members straddling every target by ±1: each errs by 1, their mean
        // is exact, so Ē = 1, E = 0 and Ā = 1.
        let targets = [0.0_f32, 2.0, -1.0, 0.5];
        let a: Vec<f32> = targets.iter().map(|t| t + 1.0).collect();
        let b: Vec<f32> = targets.iter().map(|t| t - 1.0).collect();
        let s = stats_for(&[a.clone(), b.clone()], &targets);
        let errs = vec![
            ("a".to_string(), mse(&a, &targets)),
            ("b".to_string(), mse(&b, &targets)),
        ];
        let r = s.report(&errs, None).unwrap();
        assert_eq!(r.mean_member_error, 1.0);
        assert_eq!(r.ensemble_error, 0.0);
        assert_eq!(r.ambiguity, 1.0);
        assert_eq!(r.ambiguity, r.mean_member_error - r.ensemble_error);
        assert_eq!(r.best_member_error, 1.0);
        assert!(r.beats_best_member);
    }

    #[test]
    fn ensemble_is_never_worse_than_the_mean_member_when_members_disagree() {
        let targets: Vec<f32> = (0..97).map(|r| ((r as f32) * 0.37).sin()).collect();
        let members: Vec<Vec<f32>> = (0..6)
            .map(|m| {
                targets
                    .iter()
                    .enumerate()
                    .map(|(r, t)| t + 0.3 * (((r * 7 + m * 13) as f32) * 0.91).cos())
                    .collect()
            })
            .collect();
        let s = stats_for(&members, &targets);
        let errs: Vec<(String, f64)> = members
            .iter()
            .enumerate()
            .map(|(m, o)| (format!("m{m}"), mse(o, &targets)))
            .collect();
        for k in 1..=6 {
            let r = s.report(&errs, Some(k)).unwrap();
            assert!(
                r.ensemble_error <= r.mean_member_error + 1e-15,
                "k={k}: {r:?}"
            );
            assert!(r.ambiguity >= -1e-15);
            // The Gram form agrees with a direct average over the same members.
            let idx: Vec<usize> = r
                .member_keys
                .iter()
                .map(|k| k[1..].parse().unwrap())
                .collect();
            let direct: f64 = (0..targets.len())
                .map(|row| {
                    let mean = idx.iter().map(|&a| f64::from(members[a][row])).sum::<f64>()
                        / idx.len() as f64;
                    (f64::from(targets[row]) - mean).powi(2)
                })
                .sum::<f64>()
                / targets.len() as f64;
            assert!((direct - r.ensemble_error).abs() < 1e-12, "k={k}");
        }
        assert!(s.report(&errs, Some(6)).unwrap().ambiguity > 0.0);
    }

    #[test]
    fn top_k_takes_the_lowest_error_members() {
        let targets = [1.0_f32, 1.0];
        let members = vec![vec![1.5_f32, 1.5], vec![1.0, 1.0], vec![3.0, 3.0]];
        let s = stats_for(&members, &targets);
        let errs: Vec<(String, f64)> = members
            .iter()
            .enumerate()
            .map(|(m, o)| (format!("m{m}"), mse(o, &targets)))
            .collect();
        let r = s.report(&errs, Some(2)).unwrap();
        assert_eq!(r.member_keys, vec!["m1", "m0"]);
        assert_eq!(r.members, 2);
        assert_eq!(r.best_member_error, 0.0);
        assert_eq!(r.ensemble_error, 0.0625);
        assert!(!r.beats_best_member);
        assert!(s.report(&errs, Some(0)).is_err());
        assert!(s.report(&errs, Some(4)).is_err());
    }

    #[test]
    fn merging_partials_equals_one_pass() {
        let targets: Vec<f32> = (0..20).map(|r| r as f32 * 0.1).collect();
        let members = vec![
            targets.iter().map(|t| t * 0.9).collect::<Vec<_>>(),
            targets.iter().map(|t| t + 0.2).collect(),
        ];
        let whole = stats_for(&members, &targets);
        let mut a = EnsembleStats::new(2, 1);
        let mut b = EnsembleStats::new(2, 1);
        for (r, &t) in targets.iter().enumerate() {
            let part = if r < 7 { &mut a } else { &mut b };
            part.add_record(&[t], |m, _| members[m][r]);
        }
        a.merge(&b);
        assert!((a.ensemble_error(&[0, 1]) - whole.ensemble_error(&[0, 1])).abs() < 1e-12);
    }
}
