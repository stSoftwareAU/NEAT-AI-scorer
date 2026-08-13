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

use crate::sampling::{RecordSampler, SampleSpec};
use crate::stream_score::sampled_read_worker_count;
use neat_core::training_bin_stream::{
    for_each_read_chunk, for_each_sampled_read_chunk, sampled_read_is_worthwhile,
};
use std::path::PathBuf;

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

/// Decode little-endian `f32` bytes into `dst`.
///
/// On little-endian hosts the bit pattern is already native `f32`, so the
/// decode is a bulk `copy_nonoverlapping` (Issue #539) rather than a per-float
/// load/`from_bits` loop. Big-endian hosts still convert via `from_le_bytes`.
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
        // on little-endian hosts each 4-byte group is already a native `f32` bit
        // pattern, so a bulk copy is bit-identical to per-element `from_bits`
        // (Issue #539). Source and destination do not overlap.
        unsafe {
            let out_ptr = dst.as_mut_ptr().cast::<u8>();
            std::ptr::copy_nonoverlapping(src.as_ptr(), out_ptr, n * 4);
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

/// Emergency escape hatch back to the full sweep: `NEAT_SCORER_SAMPLED_READ=off`
/// (`0`, `false`, `no` also accepted; default on).
///
/// The sampled read returns bit-identical scores, so this exists for a host
/// where sparse reads behave badly — not as configuration.
pub const SAMPLED_READ_ENV: &str = "NEAT_SCORER_SAMPLED_READ";

/// `true` unless [`SAMPLED_READ_ENV`] switches the sampled reader off.
fn sampled_read_enabled() -> bool {
    let raw = std::env::var(SAMPLED_READ_ENV).ok();
    let (enabled, warning) =
        crate::env_tuning::parse_tuning_var(SAMPLED_READ_ENV, raw.as_deref(), true, |s| {
            match s.to_ascii_lowercase().as_str() {
                "on" | "1" | "true" | "yes" => Some(true),
                "off" | "0" | "false" | "no" => Some(false),
                _ => None,
            }
        });
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }
    enabled
}

/// True when this sweep should fetch only the sampled records instead of
/// streaming the whole corpus and filtering after decode.
///
/// A full-rate call never qualifies; a sub-sample qualifies when neat-core's
/// cost model says skipping the unkept bytes beats reading them
/// ([`sampled_read_is_worthwhile`] — sparse enough, and skipping far enough to
/// be worth a seek) and the escape hatch has not switched it off.
pub(crate) fn use_sampled_read(record_bytes: usize, sample: SampleSpec) -> bool {
    !sample.is_full()
        && sampled_read_is_worthwhile(record_bytes, sample.rate())
        && sampled_read_enabled()
}

/// Drive one sweep over `bin_files`, handing whole-record chunks to
/// `score_chunk`, and return the trailing bytes left unconsumed.
///
/// Two readers sit behind this, chosen by [`use_sampled_read`] and otherwise
/// indistinguishable to the caller:
///
/// - **Full sweep** — every byte is read, decoded, and the sampler drops the
///   unkept records after decode (the pre-existing path, and still the path a
///   full-corpus call takes).
/// - **Sampled read** — only the kept records are fetched, over a pool of
///   readers, so a 5 % call stops paying to read and decode the 95 % it throws
///   away (NEAT-AI-Lamarck#123). The records arrive already filtered, so the
///   in-loop sampler is a pass-through.
///
/// Both deliver the same records in the same order, so a creature's error sum
/// accumulates identically and the scores are **bit-identical** either way.
///
/// A non-zero return is a corpus that does not end on a record boundary; callers
/// decide whether that is a fault (a completed sweep) or expected (an aborted
/// one).
pub(crate) fn sweep_corpus(
    bin_files: &[PathBuf],
    read_buf_len: usize,
    record_bytes: usize,
    sample: SampleSpec,
    score_chunk: &mut ScoreChunkFn<'_>,
) -> Result<usize, String> {
    let mut pending: Vec<u8> = Vec::new();
    let mut head: usize = 0;
    let mut unpack_floats: Vec<f32> = Vec::new();

    if use_sampled_read(record_bytes, sample) {
        let keep = move |index: u64| sample.keeps(index);
        let mut sampler = RecordSampler::full();
        for_each_sampled_read_chunk(
            bin_files,
            read_buf_len,
            record_bytes,
            sampled_read_worker_count(),
            &keep,
            |chunk| {
                run_io_loop(
                    chunk,
                    &mut pending,
                    &mut head,
                    &mut unpack_floats,
                    record_bytes,
                    &mut sampler,
                    score_chunk,
                )
            },
        )?;
    } else {
        let mut sampler = sample.sampler();
        for_each_read_chunk(bin_files, read_buf_len, |chunk| {
            run_io_loop(
                chunk,
                &mut pending,
                &mut head,
                &mut unpack_floats,
                record_bytes,
                &mut sampler,
                score_chunk,
            )
        })?;
    }

    Ok(pending.len() - head)
}

#[cfg(test)]
mod tests {
    use super::{
        FloatBufPool, SAMPLED_READ_ENV, compact_pending_if_needed, run_io_loop, sweep_corpus,
        unpack_f32s_le, use_sampled_read,
    };
    use crate::sampling::{RecordSampler, SampleSpec};
    use std::path::PathBuf;

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

    /// Production record shape — 2 512 `f32` per record, the shape the
    /// `sampled_read_is_worthwhile` policy is measured against.
    const PROD_RECORD_VALUES: usize = 2512;
    const PROD_RECORD_BYTES: usize = PROD_RECORD_VALUES * 4;

    /// Write `files` × `records_per_file` records; record `r` is `r` repeated,
    /// so a delivered float identifies its own record.
    fn write_corpus(dir: &std::path::Path, files: usize, records_per_file: usize) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let mut global = 0_u32;
        for f in 0..files {
            let mut bytes: Vec<u8> = Vec::new();
            for _ in 0..records_per_file {
                for _ in 0..PROD_RECORD_VALUES {
                    bytes.extend_from_slice(&(global as f32).to_le_bytes());
                }
                global += 1;
            }
            let path = dir.join(format!("{f}.bin"));
            std::fs::write(&path, &bytes).unwrap();
            paths.push(path);
        }
        paths
    }

    /// Every float `sweep_corpus` delivers, in delivery order.
    fn swept(files: &[PathBuf], sample: SampleSpec) -> Vec<f32> {
        let mut seen: Vec<f32> = Vec::new();
        let mut score_chunk = |f: &mut Vec<f32>, n: usize| -> Result<(), String> {
            assert!(n > 0, "an empty chunk must never reach score_chunk");
            assert_eq!(f.len(), n * PROD_RECORD_VALUES);
            seen.extend_from_slice(f);
            Ok(())
        };
        let trailing = sweep_corpus(
            files,
            PROD_RECORD_BYTES * 4,
            PROD_RECORD_BYTES,
            sample,
            &mut score_chunk,
        )
        .unwrap();
        assert_eq!(
            trailing, 0,
            "a whole-record corpus leaves no trailing bytes"
        );
        seen
    }

    /// The heart of it: fetching only the sampled records delivers **exactly**
    /// what reading everything and filtering after decode delivers — same
    /// records, same order, bit-identical floats. Anything else would move a
    /// creature's score.
    #[test]
    fn a_sampled_read_delivers_what_the_full_sweep_delivers() {
        let dir = tempfile::tempdir().unwrap();
        let files = write_corpus(dir.path(), 3, 40);

        // Rates whose mean skip clears the 64 KiB bar for this record size, so
        // every one of them exercises the sampled reader.
        for (rate, phase) in [(0.05, 0), (0.05, 7), (0.1, 3), (0.125, 1)] {
            let sample = SampleSpec::new(rate, phase).unwrap();
            assert!(
                use_sampled_read(PROD_RECORD_BYTES, sample),
                "rate {rate} on production records must take the sampled reader"
            );

            let sampled = swept(&files, sample);

            // Same corpus, same spec, forced down the full-sweep path.
            unsafe { std::env::set_var(SAMPLED_READ_ENV, "off") };
            assert!(!use_sampled_read(PROD_RECORD_BYTES, sample));
            let full = swept(&files, sample);
            unsafe { std::env::remove_var(SAMPLED_READ_ENV) };

            assert_eq!(sampled, full, "rate {rate} phase {phase} moved the records");
            assert!(!sampled.is_empty(), "the sample kept nothing to compare");
        }
    }

    /// A full-corpus call is untouched by any of this: it reads every record,
    /// in order, down the same path it always did.
    #[test]
    fn a_full_corpus_sweep_reads_every_record() {
        let dir = tempfile::tempdir().unwrap();
        let files = write_corpus(dir.path(), 2, 9);
        assert!(!use_sampled_read(PROD_RECORD_BYTES, SampleSpec::full()));

        let seen = swept(&files, SampleSpec::full());
        let records: Vec<f32> = seen
            .chunks_exact(PROD_RECORD_VALUES)
            .map(|r| r[0])
            .collect();
        assert_eq!(records, (0..18).map(|i| i as f32).collect::<Vec<_>>());
    }

    /// The policy gate, at the boundaries that decide a real call's path.
    #[test]
    fn only_sparse_samples_of_large_records_take_the_sampled_read() {
        // Production screen call: 5 % of 10 048-byte records.
        assert!(use_sampled_read(
            PROD_RECORD_BYTES,
            SampleSpec::new(0.05, 0).unwrap()
        ));
        // Full corpus — nothing to skip.
        assert!(!use_sampled_read(PROD_RECORD_BYTES, SampleSpec::full()));
        // Dense sample — the skips are too short to seek over. At this record
        // size a 20 % sample skips only 40 KiB, so it keeps the full sweep.
        assert!(!use_sampled_read(
            PROD_RECORD_BYTES,
            SampleSpec::new(0.5, 0).unwrap()
        ));
        assert!(!use_sampled_read(
            PROD_RECORD_BYTES,
            SampleSpec::new(0.2, 0).unwrap()
        ));
        // Small records — 5 % of 64 bytes skips ~1.2 KiB.
        assert!(!use_sampled_read(64, SampleSpec::new(0.05, 0).unwrap()));
    }

    /// A corpus that does not end on a record boundary is reported, not
    /// silently dropped — the caller decides whether that is a fault.
    #[test]
    fn a_ragged_corpus_reports_its_trailing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let files = write_corpus(dir.path(), 1, 4);
        let mut bytes = std::fs::read(&files[0]).unwrap();
        bytes.extend_from_slice(&[0_u8; 12]);
        std::fs::write(&files[0], &bytes).unwrap();

        let mut score_chunk = |_: &mut Vec<f32>, _: usize| -> Result<(), String> { Ok(()) };
        let trailing = sweep_corpus(
            &files,
            PROD_RECORD_BYTES * 4,
            PROD_RECORD_BYTES,
            SampleSpec::full(),
            &mut score_chunk,
        )
        .unwrap();
        assert_eq!(trailing, 12);

        // The sampled reader cannot plan reads over a ragged file at all, so it
        // fails loud rather than guessing at the record count.
        let err = sweep_corpus(
            &files,
            PROD_RECORD_BYTES * 4,
            PROD_RECORD_BYTES,
            SampleSpec::new(0.05, 0).unwrap(),
            &mut score_chunk,
        )
        .unwrap_err();
        assert!(err.contains("whole number"), "unhelpful error: {err}");
    }
}
