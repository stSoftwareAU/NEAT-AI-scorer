//! Record-level sub-sampling for the forward-only streaming reader (Issue #310).
//!
//! Multi-fidelity fitness scores each creature on a deterministic, stratified
//! **subsample** of the binary corpus instead of the full corpus. Production
//! spends ~95 % of its fitness wall-clock in the forward-only Rust batch path
//! over a single ~21 GiB corpus file, so the byte cut has to happen **inside**
//! the streaming reader — there is no shard to drop and no room for a second
//! corpus on disk.
//!
//! The stride semantics match the TypeScript consumer (NEAT-AI#3257) exactly so
//! the two agree on which records survive:
//!
//! > keep record `i` iff `floor((i+1)·rate) > floor(i·rate)`.
//!
//! This is a deterministic, stratified stride: it keeps `floor(N·rate)` of `N`
//! records spread evenly across the corpus, and — crucially — the kept set is
//! **independent of how the reader chunks the bytes**, because the decision is a
//! pure function of the record's global index. A [`RecordSampler`] threads that
//! global index through every streamed chunk.
//!
//! An optional `phase` shifts the stride to select a *different* deterministic
//! subsample (e.g. rotating the sampled stratum per generation) without any
//! randomness: the record index is offset by `phase` before the stride test.

/// Validated record sub-sampling configuration (Issue #310).
///
/// Constructed from the CLI `--sample-rate` / `--sample-phase` flags. A rate of
/// `1.0` (the default) is *full* sampling — every record is scored — and every
/// scoring path treats it as a zero-overhead pass-through.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SampleSpec {
    rate: f64,
    phase: u64,
}

impl SampleSpec {
    /// Full corpus: keep every record. This is the default when `--sample-rate`
    /// is not supplied and is a zero-overhead pass-through in the reader.
    pub const fn full() -> Self {
        Self {
            rate: 1.0,
            phase: 0,
        }
    }

    /// Build a validated spec. `rate` must lie in `(0, 1]`; `phase` shifts the
    /// deterministic stride to a different stratum (default `0`).
    ///
    /// Fails loud (Issue #3234) rather than silently clamping an out-of-range
    /// rate, so a caller that fat-fingers the flag learns immediately.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_scorer::sampling::SampleSpec;
    ///
    /// assert!(SampleSpec::new(0.25, 0).is_ok());
    /// assert!(SampleSpec::new(1.0, 0).is_ok());
    /// assert!(SampleSpec::new(0.0, 0).is_err()); // 0 excluded
    /// assert!(SampleSpec::new(1.5, 0).is_err()); // > 1 excluded
    /// ```
    pub fn new(rate: f64, phase: u64) -> Result<Self, String> {
        if !rate.is_finite() || rate <= 0.0 || rate > 1.0 {
            return Err(format!(
                "--sample-rate must be a finite value in (0, 1], got {rate}"
            ));
        }
        Ok(Self { rate, phase })
    }

    /// The sampling rate in `(0, 1]`.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// The stride phase offset.
    pub fn phase(&self) -> u64 {
        self.phase
    }

    /// `true` when this spec keeps every record (`rate == 1.0`).
    pub(crate) fn is_full(&self) -> bool {
        self.rate >= 1.0
    }

    /// Create a fresh stateful [`RecordSampler`] for one corpus sweep.
    pub fn sampler(&self) -> RecordSampler {
        RecordSampler::new(*self)
    }

    /// Create a sampler positioned at global record index `start_index`
    /// (Issue #529).
    ///
    /// Parallel per-file readers each sweep one `.bin` file, so each needs a
    /// sampler seeded with the global index of that file's first record. The
    /// stride is a pure function of the global index, so seeding selects
    /// exactly the records a single sequential sweep would have kept.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_scorer::sampling::SampleSpec;
    ///
    /// let spec = SampleSpec::new(0.5, 0).unwrap();
    /// // Records 4..8 scored by a seeked sampler keep the same global indices
    /// // {5, 7} a full sweep from 0 would have kept.
    /// let mut seeked = spec.sampler_at(4);
    /// let mut buf = vec![4.0_f32, 5.0, 6.0, 7.0];
    /// assert_eq!(seeked.filter_in_place(&mut buf, 4, 1), 2);
    /// assert_eq!(buf, vec![5.0, 7.0]);
    /// ```
    pub fn sampler_at(&self, start_index: u64) -> RecordSampler {
        RecordSampler::new_at(*self, start_index)
    }
}

impl Default for SampleSpec {
    fn default() -> Self {
        Self::full()
    }
}

/// Parse and validate a `--sample-rate` argument string into an `f64` in
/// `(0, 1]`. Used as the clap `value_parser` so an out-of-range value is
/// rejected with a clean non-zero exit rather than silently accepted.
///
/// Stays `pub` after Issue #475: the doctest below is an *external* crate, so
/// `pub(crate)` would break `cargo test --doc` (the #474 doctest-only carve-out).
///
/// # Examples
///
/// ```
/// use rust_scorer::sampling::parse_sample_rate;
///
/// assert_eq!(parse_sample_rate("0.5").unwrap(), 0.5);
/// assert!(parse_sample_rate("0").is_err());
/// assert!(parse_sample_rate("2").is_err());
/// assert!(parse_sample_rate("abc").is_err());
/// ```
pub fn parse_sample_rate(s: &str) -> Result<f64, String> {
    let rate: f64 = s
        .trim()
        .parse()
        .map_err(|_| format!("invalid --sample-rate '{s}': expected a number in (0, 1]"))?;
    // Reuse the single source of truth for range validation.
    SampleSpec::new(rate, 0).map(|spec| spec.rate())
}

/// Stateful, deterministic record sampler that threads a global record index
/// through every streamed chunk so the kept set is independent of chunk
/// boundaries (Issue #310).
///
/// Stays `pub` after Issue #475 for the same reason as [`parse_sample_rate`]:
/// [`RecordSampler::filter_in_place`] is doctested from an external crate.
#[derive(Debug, Clone)]
pub struct RecordSampler {
    spec: SampleSpec,
    /// Global index of the next record to be considered (0-based, across all
    /// `.bin` files in read order).
    next_index: u64,
}

impl RecordSampler {
    /// Build a sampler for the given spec, positioned at the first record.
    pub fn new(spec: SampleSpec) -> Self {
        Self {
            spec,
            next_index: 0,
        }
    }

    /// Build a sampler positioned at global record index `start_index`
    /// (Issue #529) — see [`SampleSpec::sampler_at`].
    pub fn new_at(spec: SampleSpec, start_index: u64) -> Self {
        Self {
            spec,
            next_index: start_index,
        }
    }

    /// A pass-through sampler that keeps every record.
    pub fn full() -> Self {
        Self::new(SampleSpec::full())
    }

    /// `true` when every record is kept (no filtering work performed).
    pub(crate) fn is_full(&self) -> bool {
        self.spec.is_full()
    }

    /// Stride predicate: keep record `i` iff
    /// `floor((j+1)·rate) > floor(j·rate)` where `j = i + phase`.
    ///
    /// Matches the TypeScript consumer's stratified stride exactly.
    fn keeps_index(&self, i: u64) -> bool {
        // f64 represents every integer index exactly up to 2^53; a 21 GiB corpus
        // holds far fewer records than that, so the cast is lossless in practice.
        let j = (i + self.spec.phase) as f64;
        ((j + 1.0) * self.spec.rate).floor() > (j * self.spec.rate).floor()
    }

    /// Consider the record at the current global index, advance past it, and
    /// return whether it should be scored. Used by the per-record recurrent
    /// path where records are not batched into a flat buffer.
    pub(crate) fn keep_next(&mut self) -> bool {
        let keep = self.keeps_index(self.next_index);
        self.next_index += 1;
        keep
    }

    /// Filter a flat buffer of `n_records` packed records
    /// (`[inputs..., targets...]` per record) **in place**, compacting the
    /// sampled records to the front and dropping the rest. Returns the kept
    /// record count; the buffer is truncated to `kept * values_per_record`.
    ///
    /// A full-rate sampler is a zero-copy pass-through: it only advances the
    /// global index and returns `n_records` unchanged.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_scorer::sampling::{RecordSampler, SampleSpec};
    ///
    /// // rate 0.5, phase 0 keeps the odd global indices {1, 3, ...}.
    /// let mut sampler = SampleSpec::new(0.5, 0).unwrap().sampler();
    /// let mut buf = vec![0.0, 0.0, 1.0, 1.0, 2.0, 2.0, 3.0, 3.0]; // 4 records, 2 values each
    /// let kept = sampler.filter_in_place(&mut buf, 4, 2);
    /// assert_eq!(kept, 2);
    /// assert_eq!(buf, vec![1.0, 1.0, 3.0, 3.0]);
    /// ```
    pub fn filter_in_place(
        &mut self,
        floats: &mut Vec<f32>,
        n_records: usize,
        values_per_record: usize,
    ) -> usize {
        if self.is_full() {
            self.next_index += n_records as u64;
            return n_records;
        }
        debug_assert_eq!(floats.len(), n_records * values_per_record);
        let mut write = 0_usize;
        for r in 0..n_records {
            if self.keeps_index(self.next_index + r as u64) {
                if write != r {
                    let src = r * values_per_record;
                    let dst = write * values_per_record;
                    floats.copy_within(src..src + values_per_record, dst);
                }
                write += 1;
            }
        }
        self.next_index += n_records as u64;
        floats.truncate(write * values_per_record);
        write
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_rejects_out_of_range_rate() {
        assert!(SampleSpec::new(0.0, 0).is_err());
        assert!(SampleSpec::new(-0.1, 0).is_err());
        assert!(SampleSpec::new(1.0001, 0).is_err());
        assert!(SampleSpec::new(f64::NAN, 0).is_err());
        assert!(SampleSpec::new(f64::INFINITY, 0).is_err());
    }

    #[test]
    fn spec_accepts_valid_rate_and_reports_full() {
        assert!(SampleSpec::new(0.5, 3).unwrap().rate() - 0.5 < 1e-12);
        assert_eq!(SampleSpec::new(0.5, 3).unwrap().phase(), 3);
        assert!(SampleSpec::full().is_full());
        assert!(SampleSpec::new(1.0, 0).unwrap().is_full());
        assert!(!SampleSpec::new(0.5, 0).unwrap().is_full());
    }

    #[test]
    fn parse_sample_rate_validates() {
        assert_eq!(parse_sample_rate("0.25").unwrap(), 0.25);
        assert_eq!(parse_sample_rate(" 1 ").unwrap(), 1.0);
        assert!(parse_sample_rate("0").is_err());
        assert!(parse_sample_rate("1.1").is_err());
        assert!(parse_sample_rate("-0.5").is_err());
        assert!(parse_sample_rate("not-a-number").is_err());
    }

    /// Reference implementation of the stride, used to cross-check the sampler.
    fn keep_ref(i: u64, rate: f64, phase: u64) -> bool {
        let j = (i + phase) as f64;
        ((j + 1.0) * rate).floor() > (j * rate).floor()
    }

    #[test]
    fn full_rate_keeps_every_record() {
        let mut sampler = RecordSampler::full();
        let mut buf: Vec<f32> = (0..12).map(|i| i as f32).collect();
        let kept = sampler.filter_in_place(&mut buf, 6, 2);
        assert_eq!(kept, 6);
        assert_eq!(buf.len(), 12);
    }

    #[test]
    fn half_rate_keeps_odd_indices() {
        // rate 0.5 phase 0 → keep {1,3,5,7,...}
        let mut sampler = SampleSpec::new(0.5, 0).unwrap().sampler();
        let n = 8_usize;
        // One value per record for simplicity: value == global index.
        let mut buf: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let kept = sampler.filter_in_place(&mut buf, n, 1);
        assert_eq!(kept, 4);
        assert_eq!(buf, vec![1.0, 3.0, 5.0, 7.0]);
    }

    #[test]
    fn quarter_rate_keeps_every_fourth() {
        let mut sampler = SampleSpec::new(0.25, 0).unwrap().sampler();
        let n = 12_usize;
        let mut buf: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let kept = sampler.filter_in_place(&mut buf, n, 1);
        // floor(12*0.25) = 3 kept: {3, 7, 11}
        assert_eq!(kept, 3);
        assert_eq!(buf, vec![3.0, 7.0, 11.0]);
    }

    #[test]
    fn kept_count_matches_floor_n_rate() {
        for &rate in &[0.1_f64, 0.25, 0.5, 0.7, 0.9] {
            let n = 1000_usize;
            let mut sampler = SampleSpec::new(rate, 0).unwrap().sampler();
            let mut buf: Vec<f32> = (0..n).map(|i| i as f32).collect();
            let kept = sampler.filter_in_place(&mut buf, n, 1);
            assert_eq!(kept as u64, (n as f64 * rate).floor() as u64, "rate {rate}");
        }
    }

    #[test]
    fn kept_set_is_independent_of_chunk_boundaries() {
        // Feeding the same records in different chunk splits must keep exactly
        // the same global indices — this is the whole point of threading a
        // global index rather than restarting the stride per chunk.
        let rate = 0.3_f64;
        let phase = 5_u64;
        let n = 100_usize;

        // Single-shot.
        let mut single = SampleSpec::new(rate, phase).unwrap().sampler();
        let mut buf_all: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let kept_all = single.filter_in_place(&mut buf_all, n, 1);
        buf_all.truncate(kept_all);

        // Uneven chunk splits: 7, 13, 1, 40, 39.
        let mut chunked = SampleSpec::new(rate, phase).unwrap().sampler();
        let mut collected: Vec<f32> = Vec::new();
        let mut start = 0_usize;
        for &len in &[7_usize, 13, 1, 40, 39] {
            let mut chunk: Vec<f32> = (start..start + len).map(|i| i as f32).collect();
            let kept = chunked.filter_in_place(&mut chunk, len, 1);
            chunk.truncate(kept);
            collected.extend_from_slice(&chunk);
            start += len;
        }

        assert_eq!(buf_all, collected);
        // And it agrees with the reference stride predicate.
        let expected: Vec<f32> = (0..n as u64)
            .filter(|&i| keep_ref(i, rate, phase))
            .map(|i| i as f32)
            .collect();
        assert_eq!(buf_all, expected);
    }

    #[test]
    fn phase_selects_a_different_subsample() {
        let rate = 0.5_f64;
        let n = 8_usize;

        let mut p0 = SampleSpec::new(rate, 0).unwrap().sampler();
        let mut b0: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let k0 = p0.filter_in_place(&mut b0, n, 1);
        b0.truncate(k0);

        let mut p1 = SampleSpec::new(rate, 1).unwrap().sampler();
        let mut b1: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let k1 = p1.filter_in_place(&mut b1, n, 1);
        b1.truncate(k1);

        // Same count, disjoint (complementary) strata for rate 0.5.
        assert_eq!(k0, k1);
        assert_ne!(b0, b1);
    }

    #[test]
    fn keep_next_matches_filter_in_place() {
        let rate = 0.4_f64;
        let phase = 2_u64;
        let n = 50_usize;

        let mut by_next = SampleSpec::new(rate, phase).unwrap().sampler();
        let kept_via_next: Vec<u64> = (0..n as u64).filter(|_| by_next.keep_next()).collect();

        let expected: Vec<u64> = (0..n as u64)
            .filter(|&i| keep_ref(i, rate, phase))
            .collect();
        assert_eq!(kept_via_next, expected);
    }

    #[test]
    fn filter_compacts_multi_value_records() {
        // 2 values per record, rate 0.5 → keep records 1 and 3.
        let mut sampler = SampleSpec::new(0.5, 0).unwrap().sampler();
        let mut buf = vec![
            0.0, 10.0, // record 0 (dropped)
            1.0, 11.0, // record 1 (kept)
            2.0, 12.0, // record 2 (dropped)
            3.0, 13.0, // record 3 (kept)
        ];
        let kept = sampler.filter_in_place(&mut buf, 4, 2);
        assert_eq!(kept, 2);
        assert_eq!(buf, vec![1.0, 11.0, 3.0, 13.0]);
    }

    /// Issue #529: a seeked sampler must keep exactly the records a single
    /// sequential sweep would have kept for the same global index range, for
    /// every split point — otherwise parallel per-file readers would score a
    /// different subset than the sequential reader.
    #[test]
    fn seeked_samplers_partition_the_sequential_kept_set() {
        let n = 97_usize;
        for &(rate, phase) in &[(0.3_f64, 0_u64), (0.25, 5), (0.75, 3), (1.0, 0)] {
            let spec = SampleSpec::new(rate, phase).unwrap();
            let expected: Vec<f32> = (0..n as u64)
                .filter(|&i| keep_ref(i, rate, phase))
                .map(|i| i as f32)
                .collect();

            for split in [1_usize, 7, 32, 96] {
                let mut kept: Vec<f32> = Vec::new();
                for (start, len) in [(0_usize, split), (split, n - split)] {
                    let mut sampler = spec.sampler_at(start as u64);
                    let mut buf: Vec<f32> = (start..start + len).map(|i| i as f32).collect();
                    let n_kept = sampler.filter_in_place(&mut buf, len, 1);
                    assert_eq!(buf.len(), n_kept);
                    kept.extend_from_slice(&buf);
                }
                assert_eq!(kept, expected, "rate {rate} phase {phase} split {split}");
            }
        }
    }

    #[test]
    fn full_rate_is_a_noop_but_advances_index() {
        // After a full-rate pass, a subsequent sub-rate sampler on a *new*
        // sampler starts fresh — but within one sampler the index must advance
        // so downstream stride decisions stay aligned.
        let mut sampler = RecordSampler::full();
        let mut buf: Vec<f32> = (0..4).map(|i| i as f32).collect();
        assert_eq!(sampler.filter_in_place(&mut buf, 4, 1), 4);
        assert_eq!(sampler.next_index, 4);
    }
}
