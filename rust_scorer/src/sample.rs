//! Record-level deterministic subsampling for the forward-only streaming reader
//! (Issue #310 — multi-fidelity fitness).
//!
//! Production spends ~95 % of its fitness wall-clock in the forward-only Rust
//! batch path (`rust_scorer <creatures_dir> <data_dir>`) over a single ~21 GiB
//! corpus file. Shard-level selection cannot help a single-file corpus, so the
//! byte cut must happen **inside** the streaming reader: this module skips whole
//! records in one pass, with no second corpus written to disk.
//!
//! ## Stratified stride semantics
//!
//! To agree bit-for-bit with the TS/WASM consumer
//! (`stSoftwareAU/NEAT-AI#3257`), a record at global index `i` is kept iff
//!
//! ```text
//! floor((i + 1) * rate) > floor(i * rate)
//! ```
//!
//! evaluated in `f64` exactly as the consumer does. With `rate == 1.0` every
//! record is kept (the difference is always `1`); with `rate == 0.5` every
//! second record is kept; with `rate == 0.25` every fourth, and so on — a
//! deterministic, evenly-spread stride rather than a random draw.
//!
//! ## Phase offset
//!
//! An optional `--sample-phase <u64>` shifts the global index by a fixed offset
//! (`i + phase`) so different evolution generations can score honest,
//! non-overlapping strata of the corpus while each run stays fully
//! deterministic. `phase == 0` (the default) reproduces the consumer formula
//! exactly. At `rate == 1.0` the phase is irrelevant — every record is kept.

/// Immutable description of a record-level subsample: the keep `rate` and a
/// deterministic `phase` offset. Cheap to copy; threaded from the CLI down to
/// each streaming scoring path, which turns it into a running [`RecordSampler`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SampleSpec {
    rate: f64,
    phase: u64,
}

impl SampleSpec {
    /// Score every record — the historical full-corpus behaviour. Equivalent to
    /// `--sample-rate 1`.
    #[must_use]
    pub fn full() -> Self {
        Self {
            rate: 1.0,
            phase: 0,
        }
    }

    /// Build a spec from a CLI `rate` (0 < rate ≤ 1) and `phase` offset.
    ///
    /// # Errors
    /// Returns `Err` when `rate` is not a finite value in the half-open-at-zero
    /// range `(0, 1]`, so a malformed `--sample-rate` fails loud (Issue #3234)
    /// rather than silently scoring the wrong number of records.
    pub fn new(rate: f64, phase: u64) -> Result<Self, String> {
        if !rate.is_finite() || rate <= 0.0 || rate > 1.0 {
            return Err(format!(
                "--sample-rate must be a finite value in (0, 1], got {rate}"
            ));
        }
        Ok(Self { rate, phase })
    }

    /// The keep rate (0 < rate ≤ 1).
    #[must_use]
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// The deterministic phase offset added to every global record index.
    // `#[allow(dead_code)]`: public API for the library surface; the binary's
    // self-contained module tree never reads it back.
    #[allow(dead_code)]
    #[must_use]
    pub fn phase(&self) -> u64 {
        self.phase
    }

    /// `true` when this spec keeps every record (`rate == 1.0`), so callers can
    /// skip the per-record filter entirely on the common full-corpus path.
    // `#[allow(dead_code)]`: public API for the library surface; the binary
    // uses [`RecordSampler::is_full`] instead.
    #[allow(dead_code)]
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.rate >= 1.0
    }

    /// Start a running [`RecordSampler`] from this spec.
    #[must_use]
    pub fn sampler(&self) -> RecordSampler {
        RecordSampler::from_spec(*self)
    }
}

/// Running record-level subsampler that filters streamed batches in place.
///
/// A single sampler is threaded across every read chunk of one corpus sweep so
/// the global record index keeps advancing between batches — the stride is
/// stratified across the *whole* corpus, not restarted per chunk.
#[derive(Debug, Clone)]
pub struct RecordSampler {
    rate: f64,
    phase: u64,
    /// Global index of the next record to be seen (0-based, before the phase
    /// offset is applied).
    next_index: u64,
}

impl RecordSampler {
    /// A pass-through sampler that keeps every record (`rate == 1.0`).
    // `#[allow(dead_code)]`: used by the library surface and `#[cfg(test)]`
    // callers; the non-test binary builds the sampler from a `SampleSpec`.
    #[allow(dead_code)]
    #[must_use]
    pub fn full() -> Self {
        Self::from_spec(SampleSpec::full())
    }

    /// Build a running sampler from an immutable [`SampleSpec`].
    #[must_use]
    pub fn from_spec(spec: SampleSpec) -> Self {
        Self {
            rate: spec.rate,
            phase: spec.phase,
            next_index: 0,
        }
    }

    /// `true` when this sampler keeps every record.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.rate >= 1.0
    }

    /// Whether the record at phase-adjusted global index `i` is kept.
    ///
    /// Matches the consumer's `f64` stratified-stride test exactly:
    /// `floor((i + 1) * rate) > floor(i * rate)`.
    #[inline]
    fn keep(&self, i: u64) -> bool {
        // Cast to f64 like the TS consumer (which uses `Number`). For corpora
        // up to a few billion records this stays well within the f64 integer
        // range, and evaluating the formula the same way keeps the two engines
        // in agreement.
        let i_f = i as f64;
        ((i_f + 1.0) * self.rate).floor() > (i_f * self.rate).floor()
    }

    /// Advance the global counter by one record and report whether that record
    /// is kept. Mirrors [`filter_in_place`](Self::filter_in_place) for consumers
    /// that iterate one record at a time (e.g. the recurrent per-record scoring
    /// loop). On the full-corpus path this always returns `true`.
    pub fn keep_next(&mut self) -> bool {
        if self.is_full() {
            self.next_index += 1;
            return true;
        }
        let global = self.next_index + self.phase;
        self.next_index += 1;
        self.keep(global)
    }

    /// Filter a packed batch of `n_records` (`[inputs..., targets...]` per
    /// record, `values_per_record` f32s each) **in place**, compacting the kept
    /// records to the front of `floats` and truncating the tail. Advances the
    /// global record counter by `n_records`. Returns the number of records kept.
    ///
    /// On the full-corpus path (`rate == 1.0`) this is a pass-through: the
    /// counter advances and the buffer is returned untouched, so no records are
    /// copied.
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

        let mut write = 0_usize;
        for r in 0..n_records {
            let global = self.next_index + r as u64 + self.phase;
            if self.keep(global) {
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
    fn spec_new_rejects_out_of_range_rates() {
        assert!(SampleSpec::new(0.0, 0).is_err());
        assert!(SampleSpec::new(-0.5, 0).is_err());
        assert!(SampleSpec::new(1.0001, 0).is_err());
        assert!(SampleSpec::new(f64::NAN, 0).is_err());
        assert!(SampleSpec::new(f64::INFINITY, 0).is_err());
    }

    #[test]
    fn spec_new_accepts_boundary_and_mid_range_rates() {
        assert!(SampleSpec::new(1.0, 0).is_ok());
        assert!(SampleSpec::new(0.5, 3).is_ok());
        // Smallest positive is fine (keeps at least the boundary records).
        assert!(SampleSpec::new(f64::MIN_POSITIVE, 0).is_ok());
    }

    #[test]
    fn full_spec_keeps_every_record() {
        let mut sampler = SampleSpec::full().sampler();
        assert!(sampler.is_full());
        // 4 records, 2 f32 each.
        let mut floats = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let kept = sampler.filter_in_place(&mut floats, 4, 2);
        assert_eq!(kept, 4);
        assert_eq!(floats, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
    }

    /// The canonical consumer formula `floor((i+1)*rate) > floor(i*rate)` at
    /// rate 0.5 keeps records 1, 3, 5, 7, ... (odd indices).
    #[test]
    fn half_rate_keeps_every_second_record_stratified() {
        let mut sampler = SampleSpec::new(0.5, 0).unwrap().sampler();
        // 8 records, 1 f32 each: value == index.
        let mut floats: Vec<f32> = (0..8).map(|i| i as f32).collect();
        let kept = sampler.filter_in_place(&mut floats, 8, 1);
        assert_eq!(kept, 4);
        assert_eq!(floats, vec![1.0, 3.0, 5.0, 7.0]);
    }

    /// Quarter rate keeps roughly one in four, evenly spread, and the stride is
    /// continuous across batch boundaries because the sampler carries its global
    /// index between calls.
    #[test]
    fn quarter_rate_stride_is_continuous_across_batches() {
        let mut sampler = SampleSpec::new(0.25, 0).unwrap().sampler();
        // First batch: records 0..8.
        let mut a: Vec<f32> = (0..8).map(|i| i as f32).collect();
        let kept_a = sampler.filter_in_place(&mut a, 8, 1);
        // Second batch: records 8..16.
        let mut b: Vec<f32> = (8..16).map(|i| i as f32).collect();
        let kept_b = sampler.filter_in_place(&mut b, 8, 1);

        // Reference: single-pass filter over 0..16 with the same formula.
        let want: Vec<f32> = (0..16)
            .filter(|&i| {
                let f = i as f64;
                ((f + 1.0) * 0.25).floor() > (f * 0.25).floor()
            })
            .map(|i| i as f32)
            .collect();
        let mut got = a;
        got.extend_from_slice(&b);
        assert_eq!(kept_a + kept_b, want.len());
        assert_eq!(got, want);
    }

    /// Phase shifts which stratum is kept without changing how many are kept.
    #[test]
    fn phase_shifts_selected_records() {
        let n = 8_usize;
        let sample = |phase: u64| -> Vec<f32> {
            let mut sampler = SampleSpec::new(0.5, phase).unwrap().sampler();
            let mut floats: Vec<f32> = (0..n).map(|i| i as f32).collect();
            let kept = sampler.filter_in_place(&mut floats, n, 1);
            assert_eq!(kept, 4);
            floats
        };
        let phase0 = sample(0);
        let phase1 = sample(1);
        assert_ne!(
            phase0, phase1,
            "a phase shift must select a different stratum"
        );
        // phase 1 keeps even indices (0,2,4,6); phase 0 keeps odd (1,3,5,7).
        assert_eq!(phase1, vec![0.0, 2.0, 4.0, 6.0]);
    }

    /// Multi-value records: filtering must move whole records, not stray floats.
    #[test]
    fn filter_preserves_whole_records() {
        let mut sampler = SampleSpec::new(0.5, 0).unwrap().sampler();
        // 4 records, 3 f32 each. Record r == [r*10, r*10+1, r*10+2].
        let mut floats: Vec<f32> = (0..4)
            .flat_map(|r| {
                [
                    r as f32 * 10.0,
                    r as f32 * 10.0 + 1.0,
                    r as f32 * 10.0 + 2.0,
                ]
            })
            .collect();
        let kept = sampler.filter_in_place(&mut floats, 4, 3);
        assert_eq!(kept, 2);
        // Keeps records 1 and 3.
        assert_eq!(floats, vec![10.0, 11.0, 12.0, 30.0, 31.0, 32.0]);
    }

    /// `keep_next` must agree with `filter_in_place` record-for-record so the
    /// per-record recurrent path and the batched forward-only path select the
    /// same subsample.
    #[test]
    fn keep_next_matches_filter_in_place() {
        let spec = SampleSpec::new(0.3, 2).unwrap();
        let n = 20_usize;

        let mut per_record = spec.sampler();
        let kept_indices: Vec<usize> = (0..n).filter(|_| per_record.keep_next()).collect();

        let mut batch = spec.sampler();
        let mut floats: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let kept = batch.filter_in_place(&mut floats, n, 1);

        assert_eq!(kept, kept_indices.len());
        let batch_indices: Vec<usize> = floats.iter().map(|&v| v as usize).collect();
        assert_eq!(batch_indices, kept_indices);
    }

    #[test]
    fn full_rate_keeps_all_regardless_of_phase() {
        let mut sampler = SampleSpec::new(1.0, 999).unwrap().sampler();
        let mut floats: Vec<f32> = (0..5).map(|i| i as f32).collect();
        let kept = sampler.filter_in_place(&mut floats, 5, 1);
        assert_eq!(kept, 5);
        assert_eq!(floats, vec![0.0, 1.0, 2.0, 3.0, 4.0]);
    }
}
