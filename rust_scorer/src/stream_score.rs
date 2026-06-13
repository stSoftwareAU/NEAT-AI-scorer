//! Chunked binary reads and fused MSE scoring for large datasets.
//!
//! Uses [`neat_core::training_bin_stream::for_each_read_chunk`] plus a `pending`
//! buffer with **head + compact**. Read tuning: **`NEAT_SCORER_READ_BYTES`**
//! (see [`crate::read_tuning::training_read_target_bytes_from_env`]).
//!
//! Optional **multi-threaded activation** for large in-memory batches (forward-only only):
//! set **`NEAT_SCORER_ACTIVATION_THREADS`** to a value `> 1` (clamped to 64). Each worker owns
//! its own [`CompiledNetwork`]; the caller-supplied template network is cloned once per
//! additional worker so activation/hint/trace scratch buffers stay independent without paying
//! a second `compile_creature` cost (Issue #42 — `CompiledNetwork: Clone` landed upstream).
//! Any batch with at least two whole records may be split across workers (very small batches
//! still pay Rayon scheduling cost). Summation order may differ slightly (floating-point).
//! JSON **`parallelActivationBatches`** counts how many batches actually used Rayon.

use std::time::Instant;

use crate::cost::{CostKind, accumulate_cost_sum};
use crate::read_tuning::{MAX_READ_BYTES, training_read_target_bytes_from_env};
use crate::stream_io::run_io_loop;
use neat_core::creature::CreatureExport;
use neat_core::network::CompiledNetwork;
use neat_core::training_bin_stream::for_each_read_chunk;
use neat_core::training_data::TrainingDataConfig;
use rayon::prelude::*;

const MAX_ACTIVATION_WORKERS: usize = 64;

/// Parsed `NEAT_SCORER_ACTIVATION_THREADS`: missing defaults to all available CPUs.
/// Clamped to `[1, MAX_ACTIVATION_WORKERS]`.
pub fn activation_worker_count_for_scorer() -> usize {
    // Unset/blank/malformed all resolve to "all available CPUs"; a malformed
    // value additionally warns instead of falling back silently (Issue #204).
    let default = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let env = std::env::var("NEAT_SCORER_ACTIVATION_THREADS").ok();
    let (parsed, warning) = crate::env_tuning::parse_tuning_var(
        "NEAT_SCORER_ACTIVATION_THREADS",
        env.as_deref(),
        default,
        |s| s.parse::<usize>().ok(),
    );
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }
    parsed.clamp(1, MAX_ACTIVATION_WORKERS)
}

/// Aligned fused read size (same rounding as [`training_read_target_bytes_from_env`]).
///
/// When **`NEAT_SCORER_ACTIVATION_THREADS` > 1**, bumps the buffer to at least **`2 * record_bytes`**
/// (capped like the core I/O tuner) so `pending` can hold two whole records per activation batch.
/// Otherwise each read often yields only one complete record when `record_bytes` is large, and
/// parallel activation never runs (`parallelActivationBatches: 0`).
pub fn effective_fused_read_buf_len(record_bytes: usize, target_read_bytes: usize) -> usize {
    let rb = record_bytes.max(1);
    let worker_count = activation_worker_count_for_scorer();
    let mut len = (target_read_bytes / rb * rb).max(rb);
    if worker_count > 1 {
        len = len.max(rb.saturating_mul(2));
    }
    let capped = (MAX_READ_BYTES / rb) * rb;
    len.min(capped.max(rb))
}

/// Split `records` (layout `[inputs..., targets...]` per record) into `workers` contiguous slices.
/// `workers` must be `<= n_records` so every slice is non-empty.
fn partition_packed_records(
    records: &[f32],
    values_per_record: usize,
    n_records: usize,
    workers: usize,
) -> Vec<&[f32]> {
    debug_assert!(workers > 0 && workers <= n_records);
    debug_assert_eq!(records.len(), n_records * values_per_record);
    let base = n_records / workers;
    let rem = n_records % workers;
    let mut out = Vec::with_capacity(workers);
    let mut record_off = 0_usize;
    for w in 0..workers {
        let take = base + usize::from(w < rem);
        let start = record_off * values_per_record;
        let end = (record_off + take) * values_per_record;
        out.push(&records[start..end]);
        record_off += take;
    }
    out
}

/// Accumulate fused cost-function sums over all `.bin` files using env-tuned
/// chunked reads. Issue #121 generalises this from MSE-only to dispatch via
/// [`accumulate_cost_sum`] so every supported [`CostKind`] runs through the
/// same I/O envelope.
///
/// Returns `(loss_sum, record_count, parallel_activation_batches, max_records_per_batch, clone_time_secs)`.
/// `clone_time_secs` covers any per-worker `CompiledNetwork` clones for activation parallelism
/// (always `0.0` when `activation_threads <= 1`). The fourth value is the largest `n_records`
/// seen in one activation call (diagnostic: if it stays `1` while `parallel_activation_batches`
/// is `0`, each read chunk holds at most one full record — raise **`NEAT_SCORER_READ_BYTES`**
/// so multiple records fit in `pending` at once).
pub fn accumulate_cost_sum_forward_only_fused(
    cost: CostKind,
    bin_files: &[std::path::PathBuf],
    config: &TrainingDataConfig,
    _creature: &CreatureExport,
    network: &mut CompiledNetwork,
) -> Result<(f64, usize, usize, usize, f64), String> {
    let record_bytes = config.bytes_per_record();
    if record_bytes == 0 {
        return Err("Invalid record byte length (zero)".to_string());
    }

    // Issue #121: validate the cost is dispatchable up-front. Every built-in
    // cost dispatches today (Issue #134 wired the last one, `CATEGORICAL_ERROR`);
    // the probe stays so a future kernel-only cost that surfaces an error here
    // is caught before any bytes are read. Issue #200: the inner par_iter now
    // propagates any per-chunk error via `?` rather than `.expect`, so a
    // content-dependent failure also returns a clean `Err`.
    accumulate_cost_sum(
        cost,
        network,
        &[],
        config.num_inputs,
        config.num_outputs,
        true,
    )?;

    let values_per_record = config.num_inputs + config.num_outputs;
    let target_read_bytes = training_read_target_bytes_from_env(record_bytes);
    let read_buf_len = effective_fused_read_buf_len(record_bytes, target_read_bytes);

    // For multi-threaded activation, each worker needs an independent `CompiledNetwork`
    // (separate activation/hint/trace buffers). `CompiledNetwork: Clone` landed upstream
    // (NEAT-AI-core#11), so we now clone the caller-supplied template once per extra worker
    // instead of paying a second `compile_creature` per thread (Issue #42).
    let worker_count = activation_worker_count_for_scorer();
    let clone_started = Instant::now();
    let mut parallel_networks: Option<Vec<CompiledNetwork>> = if worker_count > 1 {
        let mut nets = Vec::with_capacity(worker_count);
        // Reuse the caller-supplied compiled network as the first worker; clone for the rest.
        nets.push(network.clone());
        for _ in 1..worker_count {
            nets.push(network.clone());
        }
        Some(nets)
    } else {
        None
    };
    let clone_time_secs = if worker_count > 1 {
        clone_started.elapsed().as_secs_f64()
    } else {
        0.0
    };

    let mut pending: Vec<u8> = Vec::new();
    let mut head: usize = 0;
    let mut unpack_floats: Vec<f32> = Vec::new();
    let mut total_mse_sum = 0.0_f64;
    let mut total_records = 0_usize;
    let mut parallel_activation_batches = 0_usize;
    let mut max_records_per_activation_batch = 0_usize;

    // Score one whole-record slice teased out by the shared head-and-compact
    // loop (`run_io_loop`, Issue #203). Issue #121 dispatches every supported
    // cost through `accumulate_cost_sum`; Issue #200 propagates a per-slice cost
    // error via `?` rather than `.expect` across a Rayon worker.
    let mut score_chunk = |floats: &mut Vec<f32>, n_records: usize| -> Result<(), String> {
        // Read-only consumer: borrow the unpack buffer as a slice in place.
        let floats: &[f32] = floats;
        if floats.len() != n_records * values_per_record {
            return Err("Internal float unpack length mismatch".to_string());
        }
        max_records_per_activation_batch = max_records_per_activation_batch.max(n_records);
        total_records += n_records;
        let chunk_sum: f64 = match &mut parallel_networks {
            Some(nets) => {
                let effective_workers = worker_count.min(n_records).max(1);
                if effective_workers > 1 {
                    parallel_activation_batches += 1;
                    let slices = partition_packed_records(
                        floats,
                        values_per_record,
                        n_records,
                        effective_workers,
                    );
                    let partials: Result<Vec<f64>, String> = nets[..effective_workers]
                        .par_iter_mut()
                        .zip(slices)
                        .map(|(net, slice)| {
                            accumulate_cost_sum(
                                cost,
                                net,
                                slice,
                                config.num_inputs,
                                config.num_outputs,
                                true,
                            )
                        })
                        .collect();
                    partials?.into_iter().sum()
                } else {
                    accumulate_cost_sum(
                        cost,
                        network,
                        floats,
                        config.num_inputs,
                        config.num_outputs,
                        true,
                    )?
                }
            }
            None => accumulate_cost_sum(
                cost,
                network,
                floats,
                config.num_inputs,
                config.num_outputs,
                true,
            )?,
        };
        total_mse_sum += chunk_sum;
        Ok(())
    };

    for_each_read_chunk(bin_files, read_buf_len, |chunk| {
        run_io_loop(
            chunk,
            &mut pending,
            &mut head,
            &mut unpack_floats,
            record_bytes,
            &mut score_chunk,
        )
    })?;

    if head != pending.len() {
        return Err(format!(
            "Trailing {} bytes (incomplete record) after reading all training files",
            pending.len() - head
        ));
    }

    Ok((
        total_mse_sum,
        total_records,
        parallel_activation_batches,
        max_records_per_activation_batch,
        clone_time_secs,
    ))
}

#[cfg(test)]
mod tests {
    // `unpack_f32s_le` (and its Issue #103 OOB `should_panic` tests) moved to
    // the shared `crate::stream_io` module in Issue #203; the canonical copy of
    // those tests now lives there.
    use super::partition_packed_records;

    #[test]
    fn partition_packed_records_covers_all_and_balances() {
        let values_per_record = 3_usize;
        let n_records = 10_usize;
        let workers = 4_usize;
        let records: Vec<f32> = (0..(n_records * values_per_record))
            .map(|i| i as f32)
            .collect();
        let parts = partition_packed_records(&records, values_per_record, n_records, workers);
        assert_eq!(parts.len(), workers);
        assert_eq!(parts.iter().map(|s| s.len()).sum::<usize>(), records.len());
        // 10 / 4 => lengths 3,3,2,2 in floats => 9,9,6,6
        assert_eq!(parts[0].len(), 9);
        assert_eq!(parts[1].len(), 9);
        assert_eq!(parts[2].len(), 6);
        assert_eq!(parts[3].len(), 6);
    }
}
