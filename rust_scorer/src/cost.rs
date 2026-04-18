//! Per-record MSE for the non-fused scoring path (recurrent / `forwardOnly: false`).
//!
//! Matches the TypeScript `MSE.calculate()` mean over outputs.

/// Mean squared error across outputs for one record.
#[inline]
pub fn mse_mean_record(target: &[f32], output: &[f32]) -> f64 {
    assert_eq!(
        target.len(),
        output.len(),
        "Target and output length mismatch"
    );
    let len = output.len();
    assert!(len > 0, "Empty target/output arrays");
    let mut error = 0.0_f64;
    for i in 0..len {
        let diff = (target[i] - output[i]) as f64;
        error += diff * diff;
    }
    error / len as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mse_perfect_prediction() {
        let target = [1.0_f32, 2.0, 3.0];
        let output = [1.0_f32, 2.0, 3.0];
        let error = mse_mean_record(&target, &output);
        assert!((error - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_mse_known_value() {
        let target = [1.0_f32, 2.0, 3.0];
        let output = [0.9_f32, 2.1, 2.8];
        let error = mse_mean_record(&target, &output);
        assert!((error - 0.02).abs() < 1e-6);
    }
}
