//! Shared byte-level streaming helpers for the head-and-compact `.bin` reader.
//!
//! Both the multi-creature path ([`crate::multi_score`]) and the fused
//! forward-only path ([`crate::stream_score`]) stream training records through
//! `neat_core::training_bin_stream::for_each_read_chunk`, buffering any residual
//! bytes in a `pending` buffer with a **head + compact** scheme. Issue #203
//! hoists the byte-level helpers (`unpack_f32s_le`, `reserve_unpack_capacity`,
//! `compact_pending_if_needed`) and the `run_io_loop` fast-path/slow-path
//! driver here so a single copy is shared, keeping the Issue #103
//! out-of-bounds-safety invariant in `unpack_f32s_le` in one place.

use crate::sampling::RecordSampler;

/// Compact `pending` when the consumed prefix is large (avoids unbounded `head`).
pub(crate) const PENDING_COMPACT_HEAD_BYTES: usize = 512 * 1024;

/// `unpack_f32s_le` uses `set_len`; reserve up-front so capacity is sufficient.
#[inline]
pub(crate) fn reserve_unpack_capacity(buf: &mut Vec<f32>, n: usize) {
    let need = n.saturating_sub(buf.len());
    if buf.capacity() < n {
        buf.reserve(need);
    }
}

/// Decode little-endian `f32` bytes. On little-endian hosts, one unaligned `u32`
/// load per float; otherwise `from_le_bytes`.
///
/// # Panics
/// Panics if `src.len() != n * 4`. This check runs in both debug and release
/// builds because the `little` branch performs raw pointer arithmetic that
/// relies on the length invariant — a malformed `.bin` chunk must not be
/// allowed to drive an out-of-bounds read (Issue #103).
pub(crate) fn unpack_f32s_le(src: &[u8], dst: &mut Vec<f32>, n: usize) {
    assert_eq!(
        src.len(),
        n * 4,
        "unpack_f32s_le: src.len() ({}) != n * 4 ({})",
        src.len(),
        n * 4
    );
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
pub(crate) fn compact_pending_if_needed(pending: &mut Vec<u8>, head: &mut usize) {
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

/// Callback that scores one chunk of decoded packed records. The callee is
/// expected to update its own running totals.
///
/// The buffer is passed as `&mut Vec<f32>` (not `&[f32]`) so a consumer that
/// hands the chunk off to another thread can take ownership via
/// [`std::mem::replace`] and swap a recycled buffer back in, avoiding a
/// per-chunk clone (Issue #202). Read-only consumers simply borrow it as a
/// slice and leave it in place.
pub(crate) type ScoreChunkFn<'a> = dyn FnMut(&mut Vec<f32>, usize) -> Result<(), String> + 'a;

/// Small free-list of reusable `Vec<f32>` unpack buffers for the pipelined GPU
/// path (Issue #202). The GPU worker hands consumed buffers back here so the
/// I/O thread can swap a recycled buffer into the unpack slot instead of
/// allocating + copying a fresh `Vec` for every streamed chunk.
pub(crate) struct FloatBufPool {
    free: Vec<Vec<f32>>,
}

impl FloatBufPool {
    pub(crate) fn new() -> Self {
        Self { free: Vec::new() }
    }

    /// Hand out a buffer for the next unpack. Reuses a recycled buffer (keeping
    /// its heap allocation) when one is available, otherwise yields a fresh
    /// empty `Vec`. The caller's `unpack_f32s_le` clears it before filling, so
    /// returned buffers need not be empty.
    pub(crate) fn take(&mut self) -> Vec<f32> {
        self.free.pop().unwrap_or_default()
    }

    /// Return a consumed buffer for later reuse.
    pub(crate) fn recycle(&mut self, buf: Vec<f32>) {
        self.free.push(buf);
    }

    #[cfg(test)]
    pub(crate) fn free_len(&self) -> usize {
        self.free.len()
    }
}

/// Shared head-and-compact I/O loop reused by both the multi-creature and fused
/// forward-only streaming paths. Calls `score_chunk(floats, n_records)` for each
/// whole-record slice teased out of the streamed `chunk`.
///
/// Fast path: when nothing is buffered, score the aligned prefix of `chunk`
/// directly and only copy any trailing fragment into `pending` — saving a full
/// memcpy through `pending` on the common path (Issue #38). Slow path: residual
/// bytes are buffered in `pending`; merge the new chunk and consume whole
/// records from the head.
///
/// Issue #310: `sampler` applies record-level sub-sampling *after* decode and
/// *before* `score_chunk`, compacting the kept records to the front of the
/// unpack buffer so every downstream path (fused, multi-CPU, multi-GPU) scores
/// only the sampled subset with no second corpus on disk. Threading the sampler
/// through here keeps the kept set independent of chunk boundaries. A full-rate
/// (`--sample-rate 1`, the default) sampler is a zero-overhead pass-through.
/// When a chunk's sampled subset is empty, `score_chunk` is skipped entirely —
/// callers therefore never see a zero-record chunk.
pub(crate) fn run_io_loop(
    chunk: &[u8],
    pending: &mut Vec<u8>,
    head: &mut usize,
    unpack_floats: &mut Vec<f32>,
    record_bytes: usize,
    sampler: &mut RecordSampler,
    score_chunk: &mut ScoreChunkFn<'_>,
) -> Result<(), String> {
    // Records are packed as `record_bytes / 4` little-endian f32 values.
    let values_per_record = record_bytes / 4;
    if pending.is_empty() && *head == 0 && !chunk.is_empty() {
        let aligned_len = (chunk.len() / record_bytes) * record_bytes;
        if aligned_len > 0 {
            let num_f32 = aligned_len / 4;
            let n_records = aligned_len / record_bytes;
            unpack_f32s_le(&chunk[..aligned_len], unpack_floats, num_f32);
            let kept = sampler.filter_in_place(unpack_floats, n_records, values_per_record);
            if kept > 0 {
                score_chunk(unpack_floats, kept)?;
            }
        }
        if aligned_len < chunk.len() {
            // Buffer only the trailing fragment for the next call; `head` stays at 0.
            pending.extend_from_slice(&chunk[aligned_len..]);
        }
        return Ok(());
    }

    if !chunk.is_empty() {
        pending.extend_from_slice(chunk);
    }
    compact_pending_if_needed(pending, head);

    loop {
        let avail = pending.len() - *head;
        let complete_len = (avail / record_bytes) * record_bytes;
        if complete_len == 0 {
            break;
        }
        let num_f32 = complete_len / 4;
        let n_records = complete_len / record_bytes;
        let slice = &pending[*head..*head + complete_len];
        unpack_f32s_le(slice, unpack_floats, num_f32);
        let kept = sampler.filter_in_place(unpack_floats, n_records, values_per_record);
        if kept > 0 {
            score_chunk(unpack_floats, kept)?;
        }
        *head += complete_len;
    }

    // Drop fully-consumed `pending` so the next iteration can take the fast path
    // again, avoiding a wasteful compact next time round.
    if *head == pending.len() {
        pending.clear();
        *head = 0;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FloatBufPool, compact_pending_if_needed, run_io_loop, unpack_f32s_le};
    use crate::sampling::{RecordSampler, SampleSpec};

    #[test]
    fn float_buf_pool_reuses_recycled_buffer_allocation() {
        let mut pool = FloatBufPool::new();
        assert_eq!(pool.free_len(), 0);

        // Empty pool hands out a fresh buffer; fill it and note its allocation.
        let mut buf = pool.take();
        buf.extend_from_slice(&[1.0, 2.0, 3.0, 4.0]);
        let cap = buf.capacity();
        assert!(cap >= 4);

        // Recycling returns it to the free list...
        pool.recycle(buf);
        assert_eq!(pool.free_len(), 1);

        // ...and the next take reuses the same heap allocation (capacity kept),
        // so no fresh allocation is needed for the next chunk.
        let reused = pool.take();
        assert_eq!(reused.capacity(), cap);
        assert_eq!(pool.free_len(), 0);
    }

    #[test]
    fn float_buf_pool_take_on_empty_yields_fresh_empty_buffer() {
        let mut pool = FloatBufPool::new();
        let a = pool.take();
        assert!(a.is_empty());
        // Nothing recycled yet, so the pool is still empty.
        assert_eq!(pool.free_len(), 0);
    }

    #[test]
    fn float_buf_pool_take_swaps_into_unpack_slot_without_cloning() {
        // Mirrors the pipelined submit: swap the freshly-unpacked buffer out and
        // a recycled buffer in via `mem::replace`, then recycle it next round.
        let mut pool = FloatBufPool::new();
        let mut unpack = vec![10.0_f32, 20.0, 30.0];

        let outgoing = std::mem::replace(&mut unpack, pool.take());
        // The data left with `outgoing`; the unpack slot now holds a fresh Vec.
        assert_eq!(outgoing, vec![10.0_f32, 20.0, 30.0]);
        assert!(unpack.is_empty());

        // The consumer hands the buffer back once scored.
        pool.recycle(outgoing);
        assert_eq!(pool.free_len(), 1);
    }

    #[test]
    fn unpack_f32s_le_decodes_exact_length_buffer() {
        // Two little-endian f32s: 1.0 and -2.5.
        let mut src = Vec::new();
        src.extend_from_slice(&1.0_f32.to_le_bytes());
        src.extend_from_slice(&(-2.5_f32).to_le_bytes());
        let mut dst = Vec::new();
        unpack_f32s_le(&src, &mut dst, 2);
        assert_eq!(dst, vec![1.0_f32, -2.5_f32]);
    }

    #[test]
    #[should_panic(expected = "unpack_f32s_le: src.len()")]
    fn unpack_f32s_le_rejects_short_buffer_in_release() {
        // Length 7 with n=2 means src.len() != n*4 (=8). Must panic in
        // both debug and release builds (Issue #103) — never enter the
        // unsafe loop with a short slice.
        let src = vec![0u8; 7];
        let mut dst = Vec::new();
        unpack_f32s_le(&src, &mut dst, 2);
    }

    #[test]
    #[should_panic(expected = "unpack_f32s_le: src.len()")]
    fn unpack_f32s_le_rejects_oversize_buffer() {
        let src = vec![0u8; 9];
        let mut dst = Vec::new();
        unpack_f32s_le(&src, &mut dst, 2);
    }

    #[test]
    fn compact_pending_if_needed_shifts_tail_to_front() {
        // head past the compaction trigger relative to len → compact.
        let mut pending = vec![1u8, 2, 3, 4, 5, 6];
        let mut head = 4_usize; // head*2 == 8 >= len 6 → compact.
        compact_pending_if_needed(&mut pending, &mut head);
        assert_eq!(head, 0);
        assert_eq!(pending, vec![5u8, 6]);
    }

    #[test]
    fn compact_pending_if_needed_noop_when_head_zero() {
        let mut pending = vec![1u8, 2, 3, 4];
        let mut head = 0_usize;
        compact_pending_if_needed(&mut pending, &mut head);
        assert_eq!(head, 0);
        assert_eq!(pending, vec![1u8, 2, 3, 4]);
    }

    #[test]
    fn run_io_loop_fast_path_scores_aligned_and_buffers_remainder() {
        // record_bytes = 8 (two f32 per record). Feed 1.5 records' worth of
        // bytes so the fast path scores one whole record and buffers 4 bytes.
        let record_bytes = 8_usize;
        let mut chunk: Vec<u8> = Vec::new();
        chunk.extend_from_slice(&1.0_f32.to_le_bytes());
        chunk.extend_from_slice(&2.0_f32.to_le_bytes());
        chunk.extend_from_slice(&3.0_f32.to_le_bytes()); // trailing half-record

        let mut pending = Vec::new();
        let mut head = 0_usize;
        let mut floats = Vec::new();
        let mut sampler = RecordSampler::full();
        let mut scored: Vec<(Vec<f32>, usize)> = Vec::new();
        let mut score_chunk = |f: &mut Vec<f32>, n: usize| -> Result<(), String> {
            scored.push((f.clone(), n));
            Ok(())
        };

        run_io_loop(
            &chunk,
            &mut pending,
            &mut head,
            &mut floats,
            record_bytes,
            &mut sampler,
            &mut score_chunk,
        )
        .unwrap();

        assert_eq!(scored.len(), 1);
        assert_eq!(scored[0], (vec![1.0_f32, 2.0_f32], 1));
        // Trailing 4 bytes buffered, head untouched.
        assert_eq!(pending.len(), 4);
        assert_eq!(head, 0);
    }

    #[test]
    fn run_io_loop_slow_path_joins_buffered_remainder() {
        // After a remainder is buffered, the next chunk completes the record.
        let record_bytes = 8_usize;
        let mut pending = Vec::new();
        let mut head = 0_usize;
        let mut floats = Vec::new();
        let mut sampler = RecordSampler::full();
        let mut scored: Vec<(Vec<f32>, usize)> = Vec::new();

        // First chunk: 4 bytes (half a record) → buffered, nothing scored.
        let chunk_a = 3.0_f32.to_le_bytes().to_vec();
        // Second chunk: 4 bytes → completes the record [3.0, 4.0].
        let chunk_b = 4.0_f32.to_le_bytes().to_vec();

        {
            let mut score_chunk = |f: &mut Vec<f32>, n: usize| -> Result<(), String> {
                scored.push((f.clone(), n));
                Ok(())
            };
            run_io_loop(
                &chunk_a,
                &mut pending,
                &mut head,
                &mut floats,
                record_bytes,
                &mut sampler,
                &mut score_chunk,
            )
            .unwrap();
            // First chunk buffered a half-record; nothing scored yet.
            assert_eq!(pending.len(), 4);

            run_io_loop(
                &chunk_b,
                &mut pending,
                &mut head,
                &mut floats,
                record_bytes,
                &mut sampler,
                &mut score_chunk,
            )
            .unwrap();
        }

        assert_eq!(scored.len(), 1);
        assert_eq!(scored[0], (vec![3.0_f32, 4.0_f32], 1));
        // Fully consumed → pending reset for the next fast path.
        assert!(pending.is_empty());
        assert_eq!(head, 0);
    }

    #[test]
    fn run_io_loop_propagates_score_chunk_error() {
        let record_bytes = 8_usize;
        let chunk = {
            let mut c = Vec::new();
            c.extend_from_slice(&1.0_f32.to_le_bytes());
            c.extend_from_slice(&2.0_f32.to_le_bytes());
            c
        };
        let mut pending = Vec::new();
        let mut head = 0_usize;
        let mut floats = Vec::new();
        let mut sampler = RecordSampler::full();
        let mut score_chunk =
            |_f: &mut Vec<f32>, _n: usize| -> Result<(), String> { Err("boom".to_string()) };

        let err = run_io_loop(
            &chunk,
            &mut pending,
            &mut head,
            &mut floats,
            record_bytes,
            &mut sampler,
            &mut score_chunk,
        )
        .unwrap_err();
        assert_eq!(err, "boom");
    }

    #[test]
    fn run_io_loop_sub_sampling_drops_records_and_ignores_chunk_splits() {
        // record_bytes = 4 (one f32 per record); value == global record index.
        // rate 0.5, phase 0 keeps the odd global indices {1, 3, 5, 7}.
        let record_bytes = 4_usize;
        let mut sampler = SampleSpec::new(0.5, 0).unwrap().sampler();
        let mut pending = Vec::new();
        let mut head = 0_usize;
        let mut floats = Vec::new();
        let mut scored: Vec<f32> = Vec::new();

        {
            let mut score_chunk = |f: &mut Vec<f32>, n: usize| -> Result<(), String> {
                assert!(n > 0, "score_chunk must never see an empty sampled chunk");
                assert_eq!(f.len(), n);
                scored.extend_from_slice(f);
                Ok(())
            };

            // Feed records 0..8 in uneven byte chunks to prove the kept set is
            // independent of where the byte boundaries fall.
            let bytes = |records: std::ops::Range<usize>| -> Vec<u8> {
                let mut b: Vec<u8> = Vec::new();
                for i in records {
                    b.extend_from_slice(&(i as f32).to_le_bytes());
                }
                b
            };
            for chunk in [bytes(0..3), bytes(3..4), bytes(4..8)] {
                run_io_loop(
                    &chunk,
                    &mut pending,
                    &mut head,
                    &mut floats,
                    record_bytes,
                    &mut sampler,
                    &mut score_chunk,
                )
                .unwrap();
            }
        }

        assert_eq!(scored, vec![1.0, 3.0, 5.0, 7.0]);
    }
}
