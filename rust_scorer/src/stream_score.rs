//! Chunked binary reads and fused MSE scoring for large datasets.
//!
//! Uses `neat_core::training_bin_stream::for_each_read_chunk` (native: pipelined
//! disk reads; wasm32: same API, sequential reads) plus a `pending` buffer with **head + compact** to
//! avoid repeated `drain` memmoves. Feeds `mse_sum_batch_packed` for the same fused
//! path as WASM when `forwardOnly` is true.

use neat_core::loss::mse_sum_batch_packed;
use neat_core::network::CompiledNetwork;
use neat_core::training_bin_stream::for_each_read_chunk;
use neat_core::training_data::TrainingDataConfig;

/// Target bytes per read segment, rounded down to whole records (self-tuned).
const TARGET_READ_BYTES: usize = 2 * 1024 * 1024;

/// Compact `pending` when the consumed prefix is large (avoids unbounded `head`).
const PENDING_COMPACT_HEAD_BYTES: usize = 512 * 1024;

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

/// Accumulate fused MSE sums over all `.bin` files using pipelined reads.
///
/// Returns `(sum of per-record MSE values, total record count)` so the caller
/// can compute `avg_error = mse_sum / record_count as f64`, matching the
/// per-record `calculate_cost(Mse, ...)` aggregation.
pub fn accumulate_mse_sum_forward_only_fused(
    bin_files: &[std::path::PathBuf],
    config: &TrainingDataConfig,
    network: &mut CompiledNetwork,
) -> Result<(f64, usize), String> {
    let record_bytes = config.bytes_per_record();
    if record_bytes == 0 {
        return Err("Invalid record byte length (zero)".to_string());
    }

    let values_per_record = config.num_inputs + config.num_outputs;
    let read_buf_len = (TARGET_READ_BYTES / record_bytes * record_bytes).max(record_bytes);

    let mut pending: Vec<u8> = Vec::new();
    let mut head: usize = 0;
    let mut unpack_floats: Vec<f32> = Vec::new();
    let mut total_mse_sum = 0.0_f64;
    let mut total_records = 0_usize;

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
            let slice = &pending[head..head + complete_len];
            unpack_f32s_le(slice, &mut unpack_floats, num_f32);

            if unpack_floats.len() != n_records * values_per_record {
                return Err("Internal float unpack length mismatch".to_string());
            }

            total_records += n_records;
            let chunk_sum = mse_sum_batch_packed(
                network,
                &unpack_floats,
                config.num_inputs,
                config.num_outputs,
                true,
            );
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

    Ok((total_mse_sum, total_records))
}
