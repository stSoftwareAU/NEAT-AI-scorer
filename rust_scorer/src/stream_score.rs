//! Chunked binary reads and fused MSE scoring for large datasets.
//!
//! Uses [`neat_core::training_bin_stream::for_each_read_chunk`] plus a `pending`
//! buffer with **head + compact**. Read tuning: **`NEAT_SCORER_READ_BYTES`**
//! (see [`crate::read_tuning::training_read_target_bytes_from_env`]).
//!
//! Optional **multi-threaded activation** for large in-memory batches (forward-only only):
//! set **`NEAT_SCORER_ACTIVATION_THREADS`** to a value `> 1` (clamped to 64). Each worker owns
//! its own [`CompiledNetwork`], (re)built via [`neat_core::creature::compile_creature`] from the
//! shared [`CreatureExport`], so activations stay independent. Any batch with at least two whole
//! records may be split across workers (very small batches still pay Rayon scheduling cost).
//! Summation order may differ slightly (floating-point). JSON **`parallelActivationBatches`**
//! counts how many batches actually used Rayon.

use crate::read_tuning::{MAX_READ_BYTES, training_read_target_bytes_from_env};
use neat_core::creature::{CreatureExport, compile_creature};
use neat_core::loss::mse_sum_batch_packed;
use neat_core::network::CompiledNetwork;
use neat_core::training_bin_stream::for_each_read_chunk;
use neat_core::training_data::TrainingDataConfig;
use rayon::prelude::*;

/// Compact `pending` when the consumed prefix is large (avoids unbounded `head`).
const PENDING_COMPACT_HEAD_BYTES: usize = 512 * 1024;

const MAX_ACTIVATION_WORKERS: usize = 64;

/// Parsed `NEAT_SCORER_ACTIVATION_THREADS`: `1` or missing = sequential activation within each batch.
/// Clamped to `[1, MAX_ACTIVATION_WORKERS]`.
pub fn activation_worker_count_for_scorer() -> usize {
    match std::env::var("NEAT_SCORER_ACTIVATION_THREADS") {
        Ok(s) => {
            let t = s.trim().parse::<usize>().unwrap_or(1);
            t.clamp(1, MAX_ACTIVATION_WORKERS)
        }
        Err(_) => 1,
    }
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

/// `unpack_f32s_le` uses `set_len`; reserve up-front so capacity is sufficient.
#[inline]
fn reserve_unpack_capacity(buf: &mut Vec<f32>, n: usize) {
    if buf.capacity() < n {
        buf.reserve(n - buf.capacity());
    }
}

/// Decode little-endian `f32` bytes. On little-endian hosts, one unaligned `u32`
/// load per float; otherwise `from_le_bytes`.
///
/// # Safety (`little` branch)
/// Caller must ensure `src.len() == n * 4` and `buf` has capacity ≥ `n` before
/// the unsafe `set_len` / writes.
fn unpack_f32s_le(src: &[u8], dst: &mut Vec<f32>, n: usize) {
    debug_assert_eq!(src.len(), n * 4);
    dst.clear();
    reserve_unpack_capacity(dst, n);

    #[cfg(target_endian = "little")]
    {
        // SAFETY: `src.len() == n * 4`, capacity ≥ `n` after `reserve_unpack_capacity`;
        // we initialise all `n` elements before `set_len(n)`.
        unsafe {
            let out_ptr = dst.as_mut_ptr();
            let p = src.as_ptr();
            for i in 0..n {
                let bits = p.add(i * 4).cast::<u32>().read_unaligned();
                out_ptr.add(i).write(f32::from_bits(bits));
            }
            dst.set_len(n);
        }
    }

    #[cfg(not(target_endian = "little"))]
    {
        for q in src.chunks_exact(4) {
            dst.push(f32::from_le_bytes([q[0], q[1], q[2], q[3]]));
        }
    }
}

#[inline]
fn compact_pending_if_needed(pending: &mut Vec<u8>, head: &mut usize) {
    if *head == 0 {
        return;
    }
    let should_compact = *head >= PENDING_COMPACT_HEAD_BYTES || *head * 2 >= pending.len();
    if !should_compact {
        return;
    }
    let tail = pending.len() - *head;
    pending.copy_within(*head.., 0);
    pending.truncate(tail);
    *head = 0;
}

/// Accumulate fused MSE sums over all `.bin` files using env-tuned chunked reads.
///
/// Returns `(mse_sum, record_count, parallel_activation_batches, max_records_per_batch)`.
/// The last value is the largest `n_records` seen in one activation call (diagnostic: if it stays
/// `1` while `parallel_activation_batches` is `0`, each read chunk holds at most one full record —
/// raise **`NEAT_SCORER_READ_BYTES`** so multiple records fit in `pending` at once).
pub fn accumulate_mse_sum_forward_only_fused(
    bin_files: &[std::path::PathBuf],
    config: &TrainingDataConfig,
    creature: &CreatureExport,
    network: &mut CompiledNetwork,
) -> Result<(f64, usize, usize, usize), String> {
    let record_bytes = config.bytes_per_record();
    if record_bytes == 0 {
        return Err("Invalid record byte length (zero)".to_string());
    }

    let values_per_record = config.num_inputs + config.num_outputs;
    let target_read_bytes = training_read_target_bytes_from_env(record_bytes);
    let read_buf_len = effective_fused_read_buf_len(record_bytes, target_read_bytes);

    // For multi-threaded activation, each worker needs an independent `CompiledNetwork`
    // (separate activation/hint/trace buffers). Re-compile from the shared creature export
    // rather than relying on `CompiledNetwork: Clone` (NEAT-AI-core#11 — not yet landed).
    let worker_count = activation_worker_count_for_scorer();
    let mut parallel_networks: Option<Vec<CompiledNetwork>> = if worker_count > 1 {
        let mut nets = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            nets.push(compile_creature(creature)?);
        }
        Some(nets)
    } else {
        None
    };

    let mut pending: Vec<u8> = Vec::new();
    let mut head: usize = 0;
    let mut unpack_floats: Vec<f32> = Vec::new();
    let mut total_mse_sum = 0.0_f64;
    let mut total_records = 0_usize;
    let mut parallel_activation_batches = 0_usize;
    let mut max_records_per_activation_batch = 0_usize;

    for_each_read_chunk(bin_files, read_buf_len, |chunk| {
        if !chunk.is_empty() {
            pending.extend_from_slice(chunk);
        }
        compact_pending_if_needed(&mut pending, &mut head);

        loop {
            let avail = pending.len() - head;
            let complete_len = (avail / record_bytes) * record_bytes;
            if complete_len == 0 {
                break;
            }

            let num_f32 = complete_len / 4;
            let n_records = complete_len / record_bytes;
            max_records_per_activation_batch = max_records_per_activation_batch.max(n_records);
            let slice = &pending[head..head + complete_len];
            unpack_f32s_le(slice, &mut unpack_floats, num_f32);

            if unpack_floats.len() != n_records * values_per_record {
                return Err("Internal float unpack length mismatch".to_string());
            }

            total_records += n_records;
            let chunk_sum = match &mut parallel_networks {
                Some(nets) => {
                    let effective_workers = worker_count.min(n_records).max(1);
                    if effective_workers > 1 {
                        parallel_activation_batches += 1;
                        let slices = partition_packed_records(
                            &unpack_floats,
                            values_per_record,
                            n_records,
                            effective_workers,
                        );
                        nets[..effective_workers]
                            .par_iter_mut()
                            .zip(slices)
                            .map(|(net, slice)| {
                                mse_sum_batch_packed(
                                    net,
                                    slice,
                                    config.num_inputs,
                                    config.num_outputs,
                                    true,
                                )
                            })
                            .sum()
                    } else {
                        mse_sum_batch_packed(
                            network,
                            &unpack_floats,
                            config.num_inputs,
                            config.num_outputs,
                            true,
                        )
                    }
                }
                None => mse_sum_batch_packed(
                    network,
                    &unpack_floats,
                    config.num_inputs,
                    config.num_outputs,
                    true,
                ),
            };
            total_mse_sum += chunk_sum;
            head += complete_len;
        }
        Ok(())
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
    ))
}

#[cfg(test)]
mod tests {
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
