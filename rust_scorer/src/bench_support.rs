//! Shared helpers for the `src/bin/*_bench` binaries (Issue #649).
//!
//! The bench binaries each reported a median run time from their own copy of
//! the same helper; one copy lives here so the NaN handling cannot drift.

/// Median of `times` (milliseconds), sorting the slice in place.
///
/// Sorts with [`f64::total_cmp`], so a `NaN` timing is ordered rather than
/// panicking the bench. An even-length slice yields the mean of the two
/// middle values.
///
/// # Panics
///
/// Panics when `times` is empty — a bench with no timed runs has no median.
pub fn median_ms(times: &mut [f64]) -> f64 {
    assert!(!times.is_empty(), "median_ms needs at least one timing");
    times.sort_by(f64::total_cmp);
    let mid = times.len() / 2;
    if times.len() % 2 == 1 {
        times[mid]
    } else {
        (times[mid - 1] + times[mid]) / 2.0
    }
}

/// Distribution-free 95 % confidence interval for the median of `sorted`
/// (ascending), from order statistics (Issue #683).
///
/// The interval `[x_(j), x_(n+1-j)]` (1-based ranks) covers the population
/// median with probability `1 − 2·P(B ≤ j − 1)`, `B ~ Binomial(n, ½)`. This
/// returns the narrowest such interval with coverage ≥ 95 % — the largest `j`
/// with `P(B ≤ j − 1) ≤ 2.5 %` — as `(low, high, coverage)`, or `None` when even
/// the full range falls short (`n ≤ 5`: its coverage is `1 − 2/2ⁿ`).
#[must_use]
pub fn median_ci95(sorted: &[f64]) -> Option<(f64, f64, f64)> {
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    // cdf[i] = P(B <= i).
    let mut cdf = Vec::with_capacity(n + 1);
    let mut choose = 1.0_f64;
    let mut acc = 0.0_f64;
    let half_n = 0.5_f64.powi(n as i32);
    for i in 0..=n {
        acc += choose * half_n;
        cdf.push(acc);
        choose = choose * (n - i) as f64 / (i + 1) as f64;
    }
    // Largest j >= 1 with cdf[j - 1] <= 0.025.
    let j = (1..=n.div_ceil(2))
        .take_while(|&j| cdf[j - 1] <= 0.025)
        .last()?;
    let coverage = 1.0 - 2.0 * cdf[j - 1];
    Some((sorted[j - 1], sorted[n - j], coverage))
}

#[cfg(test)]
mod tests {
    use super::{median_ci95, median_ms};

    #[test]
    fn odd_length_returns_middle_value() {
        assert_eq!(median_ms(&mut [3.0, 1.0, 2.0]), 2.0);
    }

    #[test]
    fn even_length_returns_mean_of_middle_pair() {
        assert_eq!(median_ms(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
    }

    #[test]
    fn single_value_is_its_own_median() {
        assert_eq!(median_ms(&mut [7.25]), 7.25);
    }

    #[test]
    fn sorts_the_slice_in_place() {
        let mut times = [5.0, -1.0, 3.0];
        median_ms(&mut times);
        assert_eq!(times, [-1.0, 3.0, 5.0]);
    }

    #[test]
    fn nan_timing_does_not_panic() {
        // `total_cmp` orders positive NaN after every finite value, so the
        // median of the finite majority is still reported.
        let mut times = [2.0, f64::NAN, 1.0, 3.0, 4.0];
        assert_eq!(median_ms(&mut times), 3.0);
        assert!(times[4].is_nan());
    }

    #[test]
    fn identical_values_return_that_value() {
        assert_eq!(median_ms(&mut [1.5; 6]), 1.5);
    }

    #[test]
    #[should_panic(expected = "at least one timing")]
    fn empty_slice_panics_loudly() {
        median_ms(&mut []);
    }

    fn ranks(n: usize) -> Option<(f64, f64, f64)> {
        let sorted: Vec<f64> = (1..=n).map(|r| r as f64).collect();
        median_ci95(&sorted)
    }

    #[test]
    fn five_runs_have_no_95_percent_median_interval() {
        // Full range covers only 1 - 2/32 = 93.75 %.
        assert_eq!(ranks(5), None);
        assert_eq!(ranks(1), None);
        assert_eq!(median_ci95(&[]), None);
    }

    #[test]
    fn six_runs_give_the_full_range() {
        assert_eq!(ranks(6), Some((1.0, 6.0, 1.0 - 2.0 / 64.0)));
        assert_eq!(ranks(8), Some((1.0, 8.0, 1.0 - 2.0 / 256.0)));
    }

    #[test]
    fn nine_runs_give_the_second_and_eighth_values() {
        let (lo, hi, coverage) = ranks(9).unwrap();
        assert_eq!((lo, hi), (2.0, 8.0));
        assert!((coverage - (1.0 - 2.0 * 10.0 / 512.0)).abs() < 1e-15);
        assert!(coverage >= 0.95);
    }

    #[test]
    fn coverage_is_at_least_95_percent_whenever_an_interval_exists() {
        for n in 6..=60 {
            let (_, _, c) = ranks(n).unwrap();
            assert!(c >= 0.95, "n={n}: {c}");
        }
    }
}
