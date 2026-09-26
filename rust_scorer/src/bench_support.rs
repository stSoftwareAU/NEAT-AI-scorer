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

#[cfg(test)]
mod tests {
    use super::median_ms;

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
}
