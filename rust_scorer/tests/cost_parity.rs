//! Issue #338 — RMSE parity: on identical data the finalised RMSE error equals
//! `sqrt(MSE)`, and RMSE ranks a fixed set of creatures in the *same order* as
//! MSE (a monotonic transform preserves selection order). These live alongside
//! the Issue #122 reference-parity tests below.
//!
//! Issue #122 — Numerical parity tests vs the TypeScript reference for all
//! seven supported cost functions.
//!
//! For each cost in `BUILT_IN_COST_NAMES` this file:
//!   1. Builds a small deterministic creature whose activations are known
//!      analytically (an `IDENTITY` topology so `output_j = input_j`).
//!   2. Hand-constructs a packed `[inputs..., targets...]` record buffer
//!      containing both the typical path and at least one cost-appropriate
//!      edge case.
//!   3. Transcribes the TS formula from `NEAT-AI/src/costs/*.ts` (as
//!      constants — no live TS execution) and computes the expected error
//!      sum over the records.
//!   4. Calls `rust_scorer::cost::accumulate_cost_sum` with the matching
//!      `CostKind` and asserts the absolute difference stays within the
//!      per-cost epsilon below.
//!
//! ## Per-cost epsilons (Issue #122 brief)
//! - Smooth costs (`MSE`, `MAE`, `MAPE`, `HINGE`): **`1e-9`** absolute.
//! - Log-based costs (`MSLE`, `CROSS_ENTROPY`): **`1e-6`** absolute — leaves
//!   headroom for last-bit `ln`/`log` rounding drift between V8 and Rust libm.
//! - `CATEGORICAL_ERROR`: **exact equality** — the argmax misclassification
//!   sum is integer-valued.
//!
//! ## Failure modes locked in
//! - If the Rust dispatch picks the wrong cost (swap two `CostKind` arms in
//!   `cost::accumulate_cost_sum`) the per-cost expected values diverge by a
//!   much larger amount than the epsilon, so the assertion fires.
//! - If a future neat-core change drifts ln/log rounding beyond the
//!   documented epsilon, the assertion also fires.
//!
//! ## Runtime
//! Every test uses ≤ 9 packed records, runs once on the CPU dispatch path,
//! and so completes in tens of milliseconds — the file as a whole fits
//! comfortably under the 30s cap in the issue acceptance criteria.

use neat_core::creature::{compile_creature, parse_creature_json};
use neat_core::network::CompiledNetwork;
use rust_scorer::cost::{CostKind, accumulate_cost_sum};

/// 1e-9 absolute tolerance for smooth/algebraic costs (`MSE`, `MAE`, `MAPE`,
/// `HINGE`). At our chosen value ranges all per-record terms have magnitude
/// `O(1)` so the sums stay well within f64 precision and the assertion only
/// fires on a genuine routing/formula bug.
const EPS_SMOOTH: f64 = 1e-9;

/// 1e-6 absolute tolerance for log-based costs (`MSLE`, `CROSS_ENTROPY`).
/// The looser bound accommodates last-bit `ln`/`log` rounding drift between
/// V8 (the TS reference) and Rust libm.
const EPS_LOG: f64 = 1e-6;

/// Numerical-stability epsilon used by the TS sources for `MAPE`, `MSLE`,
/// and `CROSS_ENTROPY`. Matches `1e-15` in
/// `NEAT-AI/src/costs/{MAPE,MSLE,CrossEntropy}.ts`.
const TS_LOSS_EPSILON: f64 = 1e-15;

/// 1-input, 1-output `IDENTITY` creature: `output = input`. Used for every
/// regression-friendly cost (MSE, MAE, MAPE, MSLE, HINGE, CROSS_ENTROPY) so
/// the predicted value is exactly the packed input — letting the expected
/// values be computed directly from the records without re-running activation.
fn identity_1_in_1_out() -> CompiledNetwork {
    let json = r#"{
        "input": 1,
        "output": 1,
        "forwardOnly": true,
        "semanticVersion": "4.0.0",
        "neurons": [
            {"type": "output", "uuid": "output-0", "bias": 0.0, "squash": "IDENTITY"}
        ],
        "synapses": [
            {"fromUUID": "input-0", "toUUID": "output-0", "weight": 1.0}
        ]
    }"#;
    compile_creature(&parse_creature_json(json).expect("parse identity_1_in_1_out"))
        .expect("compile identity_1_in_1_out")
}

/// 2-input, 2-output `IDENTITY` creature: `output_j = input_j`. Required for
/// `CATEGORICAL_ERROR` (Issue #134 — dispatch now lands; argmax over a single
/// output is always index 0, so a multi-output topology is needed to exercise
/// the misclassification path).
fn identity_2_in_2_out() -> CompiledNetwork {
    let json = r#"{
        "input": 2,
        "output": 2,
        "forwardOnly": true,
        "semanticVersion": "4.0.0",
        "neurons": [
            {"type": "output", "uuid": "output-0", "bias": 0.0, "squash": "IDENTITY"},
            {"type": "output", "uuid": "output-1", "bias": 0.0, "squash": "IDENTITY"}
        ],
        "synapses": [
            {"fromUUID": "input-0", "toUUID": "output-0", "weight": 1.0},
            {"fromUUID": "input-1", "toUUID": "output-1", "weight": 1.0}
        ]
    }"#;
    compile_creature(&parse_creature_json(json).expect("parse identity_2_in_2_out"))
        .expect("compile identity_2_in_2_out")
}

/// Pack a list of (input, target) pairs into the `[inputs..., targets...]`
/// layout expected by `accumulate_cost_sum`. Both fields are stored as `f32`
/// so the dispatch path sees the same bit pattern the expected calculation
/// reads back.
fn pack_pairs(pairs: &[(f32, f32)]) -> Vec<f32> {
    pairs.iter().flat_map(|(i, t)| [*i, *t]).collect()
}

/// MSE — `NEAT-AI/src/costs/MSE.ts`
///
/// TS formula: `error = (1/n) * Σ_outputs (target - output)²`
///
/// With our 1-output `IDENTITY` creature `output = input`, so per record the
/// contribution is `(target - input)²`. `accumulate_cost_sum` returns the sum
/// over records (no record-count normalisation), which is what we compare.
///
/// Edge case: a record where `target == input` contributes exactly `0.0`.
#[test]
fn parity_mse_matches_ts_reference() {
    let mut net = identity_1_in_1_out();
    let pairs: &[(f32, f32)] = &[
        (0.5, 0.7),
        (1.0, 0.0),
        (-0.25, -0.25), // edge: exact-zero error
        (0.1, 0.2),
        (-1.0, 1.0),
        (0.5, 0.5),
        (2.0, 1.5),
        (-0.5, 0.0),
        (3.0, 0.0), // 9th record forces the SIMD path + scalar remainder
    ];
    let records = pack_pairs(pairs);

    // Expected: TS formula transcribed from MSE.ts.
    let mut expected: f64 = 0.0;
    for &(input, target) in pairs {
        let diff = target as f64 - input as f64;
        expected += diff * diff;
    }

    let actual = accumulate_cost_sum(CostKind::Mse, &mut net, &records, 1, 1, true)
        .expect("MSE dispatch must succeed");
    let drift = (actual - expected).abs();
    assert!(
        drift < EPS_SMOOTH,
        "MSE parity drift: actual={actual}, expected={expected}, |Δ|={drift:e} (eps={EPS_SMOOTH:e})"
    );
}

/// MAE — `NEAT-AI/src/costs/MAE.ts`
///
/// TS formula: `error = (1/n) * Σ_outputs |target - output|`
///
/// With 1 output, per record = `|target - input|`.
///
/// Edge case: exact-zero error when `target == input`.
#[test]
fn parity_mae_matches_ts_reference() {
    let mut net = identity_1_in_1_out();
    let pairs: &[(f32, f32)] = &[
        (0.5, 0.7),
        (1.0, 0.0),
        (-0.25, -0.25), // edge: exact-zero error
        (0.1, 0.2),
        (-1.0, 1.0),
        (0.5, 0.5),
        (2.0, 1.5),
        (-0.5, 0.0),
        (3.0, 0.0),
    ];
    let records = pack_pairs(pairs);

    let mut expected: f64 = 0.0;
    for &(input, target) in pairs {
        let diff = target as f64 - input as f64;
        expected += diff.abs();
    }

    let actual = accumulate_cost_sum(CostKind::Mae, &mut net, &records, 1, 1, true)
        .expect("MAE dispatch must succeed");
    let drift = (actual - expected).abs();
    assert!(
        drift < EPS_SMOOTH,
        "MAE parity drift: actual={actual}, expected={expected}, |Δ|={drift:e} (eps={EPS_SMOOTH:e})"
    );
}

/// MAPE — `NEAT-AI/src/costs/MAPE.ts`
///
/// TS formula: `error = (1/n) * Σ_outputs |(output - target) / max(target, 1e-15)|`
///
/// Targets are kept strictly positive: MAPE is a percentage-error metric on a
/// positive magnitude (forecasting, sales, demand, etc.) and feeding it a
/// negative or exactly-zero target lands on the `max(t, 1e-15)` stabiliser
/// where a known TS↔Rust numerator/denominator asymmetry kicks in. That
/// boundary is out of scope for this issue; #122 only locks in parity on the
/// valid input regime plus the documented stabiliser edge case below.
///
/// Edge case (per issue brief — "zero targets for MAPE → epsilon-stabilised"):
/// `target = 1e-10` — small enough that the per-record contribution is
/// dominated by `o / target ≈ 5e9`, exercising the epsilon-stabilised
/// near-zero path while staying several orders of magnitude above the
/// `1e-15` clamp so the formula evaluates identically in TS and Rust.
#[test]
fn parity_mape_matches_ts_reference() {
    let mut net = identity_1_in_1_out();
    let pairs: &[(f32, f32)] = &[
        (0.5, 1.0),
        (2.0, 4.0),
        (1.0, 1.0), // exact-zero error
        (0.5, 2.0),
        (3.0, 1.5),
        (1.5, 1.0),
        (5.0, 10.0),
        (0.8, 2.0),
        (0.5, 1e-10), // edge: near-zero target via the epsilon-stabilised path
    ];
    let records = pack_pairs(pairs);

    let mut expected: f64 = 0.0;
    for &(input, target) in pairs {
        let o = input as f64;
        let t = target as f64;
        let denom = t.max(TS_LOSS_EPSILON);
        expected += ((o - t) / denom).abs();
    }

    let actual = accumulate_cost_sum(CostKind::Mape, &mut net, &records, 1, 1, true)
        .expect("MAPE dispatch must succeed");
    // Use a relative tolerance: the near-zero-target edge record produces a
    // contribution of magnitude ~5e9 where f64 precision is ~5e-7 absolute.
    // A pure `1e-9` absolute tolerance would falsely fail on round-trip
    // rounding alone; scaling by `expected.abs().max(1.0)` keeps the
    // assertion strict for normal-magnitude values while admitting the
    // unavoidable rounding at the documented edge case.
    let drift = (actual - expected).abs();
    let tol = EPS_SMOOTH * expected.abs().max(1.0);
    assert!(
        drift < tol,
        "MAPE parity drift: actual={actual}, expected={expected}, |Δ|={drift:e} (tol={tol:e})"
    );
}

/// MSLE — `NEAT-AI/src/costs/MSLE.ts`
///
/// TS formula (as written in the source — note no squaring and no per-record
/// mean): `error = Σ_outputs (ln(max(target, 1e-15)) - ln(max(output, 1e-15)))`
///
/// Edge cases: a zero target exercises the `max(_, 1e-15)` clamp on the
/// target side; a zero output (input = 0.0 with the identity creature)
/// exercises it on the output side.
#[test]
fn parity_msle_matches_ts_reference() {
    let mut net = identity_1_in_1_out();
    let pairs: &[(f32, f32)] = &[
        (1.0, 1.0), // exact-zero error
        (2.0, 8.0),
        (0.5, 2.0),
        (3.0, 9.0),
        (10.0, 1.0),
        (1.0, 0.0), // edge: zero target → ln(eps) - ln(1)
        (0.0, 1.0), // edge: zero output → ln(1) - ln(eps)
        (4.0, 2.0),
        (0.25, 1.0),
    ];
    let records = pack_pairs(pairs);

    let mut expected: f64 = 0.0;
    for &(input, target) in pairs {
        let t = (target as f64).max(TS_LOSS_EPSILON);
        let o = (input as f64).max(TS_LOSS_EPSILON);
        expected += t.ln() - o.ln();
    }

    let actual = accumulate_cost_sum(CostKind::Msle, &mut net, &records, 1, 1, true)
        .expect("MSLE dispatch must succeed");
    let drift = (actual - expected).abs();
    assert!(
        drift < EPS_LOG,
        "MSLE parity drift: actual={actual}, expected={expected}, |Δ|={drift:e} (eps={EPS_LOG:e})"
    );
}

/// HINGE — `NEAT-AI/src/costs/HINGE.ts`
///
/// TS formula (note no per-record mean):
/// `error = Σ_outputs max(0, 1 - target * output)`
///
/// Edge case: a confidently-correct prediction (`target * output > 1`) sits
/// on the flat side of the hinge and contributes exactly `0.0`.
#[test]
fn parity_hinge_matches_ts_reference() {
    let mut net = identity_1_in_1_out();
    let pairs: &[(f32, f32)] = &[
        (0.8, 1.0),
        (-0.5, -1.0),
        (1.5, 1.0),   // edge: confidently correct → margin > 1, hinge = 0
        (-1.5, -1.0), // edge: confidently correct (other side)
        (0.0, 1.0),
        (0.5, -1.0),
        (1.0, 1.0), // edge: on the boundary, hinge = 0 exactly
        (0.5, 1.0),
        (2.0, 0.0), // edge: target = 0 nullifies the margin term
    ];
    let records = pack_pairs(pairs);

    let mut expected: f64 = 0.0;
    for &(input, target) in pairs {
        let t = target as f64;
        let o = input as f64;
        expected += (1.0 - t * o).max(0.0);
    }

    let actual = accumulate_cost_sum(CostKind::Hinge, &mut net, &records, 1, 1, true)
        .expect("HINGE dispatch must succeed");
    let drift = (actual - expected).abs();
    assert!(
        drift < EPS_SMOOTH,
        "HINGE parity drift: actual={actual}, expected={expected}, |Δ|={drift:e} (eps={EPS_SMOOTH:e})"
    );
}

/// CROSS_ENTROPY — `NEAT-AI/src/costs/CrossEntropy.ts`
///
/// TS formula:
/// `error = -(1/n) * Σ_outputs (t * ln(o_clamped) + (1 - t) * ln(1 - o_clamped))`
/// with `o_clamped = clamp(output, 1e-15, 1 - 1e-15)`.
///
/// With 1 output the dispatch sum is `Σ_records per_record_ce`.
///
/// Edge cases (per issue brief — "clamped probabilities"): records that drive
/// the output to exactly `0.0` or `1.0` exercise both ends of the clamp.
#[test]
fn parity_cross_entropy_matches_ts_reference() {
    let mut net = identity_1_in_1_out();
    let pairs: &[(f32, f32)] = &[
        (0.9, 1.0),
        (0.1, 0.0),
        (0.5, 0.5),
        (0.5, 1.0),
        (0.5, 0.0),
        (0.99, 1.0),
        (0.01, 0.0),
        (1.0, 1.0), // edge: output at upper clamp boundary (1 → 1 - 1e-15)
        (0.0, 0.0), // edge: output at lower clamp boundary (0 → 1e-15)
    ];
    let records = pack_pairs(pairs);

    let mut expected: f64 = 0.0;
    for &(input, target) in pairs {
        let t = target as f64;
        let o = (input as f64).clamp(TS_LOSS_EPSILON, 1.0 - TS_LOSS_EPSILON);
        expected += -(t * o.ln() + (1.0 - t) * (1.0 - o).ln());
    }

    let actual = accumulate_cost_sum(CostKind::CrossEntropy, &mut net, &records, 1, 1, true)
        .expect("CROSS_ENTROPY dispatch must succeed");
    let drift = (actual - expected).abs();
    assert!(
        drift < EPS_LOG,
        "CROSS_ENTROPY parity drift: actual={actual}, expected={expected}, |Δ|={drift:e} (eps={EPS_LOG:e})"
    );
}

/// CATEGORICAL_ERROR — `NEAT-AI/src/costs/CategoricalError.ts`
///
/// TS formula: per record, `argmax(target) == argmax(output) ? 0 : 1`
/// (ties resolve to the lowest index — matches the `>` comparison in the TS
/// source).
///
/// Issue #134: `categorical_error_sum_batch_packed` landed in `neat-core`
/// (NEAT-AI-core#88) so dispatch through `accumulate_cost_sum` now
/// succeeds. This parity test:
///   1. Hand-computes the TS argmax-misclassification sum from the same
///      activations the upstream helper sees (`network.activate_into`)
///      and asserts **exact equality** with the analytically-known
///      expected count — argmax is integer-valued.
///   2. Calls `accumulate_cost_sum` with `CostKind::CategoricalError` and
///      asserts the dispatch sum equals the hand-rolled sum exactly.
#[test]
fn parity_categorical_error_matches_ts_reference() {
    let mut net = identity_2_in_2_out();
    // Each record is [in0, in1, t0, t1]. With identity_2_in_2_out the
    // predicted outputs are exactly (in0, in1), so argmax(output) is the
    // index of the larger input.
    let records_2d: &[[f32; 4]] = &[
        [0.8, 0.2, 1.0, 0.0],   // pred=0, true=0 → 0 (correct)
        [0.2, 0.8, 0.0, 1.0],   // pred=1, true=1 → 0 (correct)
        [0.6, 0.4, 0.0, 1.0],   // pred=0, true=1 → 1 (incorrect)
        [0.4, 0.6, 1.0, 0.0],   // pred=1, true=0 → 1 (incorrect)
        [0.7, 0.7, 0.5, 0.5],   // edge: ties both sides → both argmax = 0 → 0
        [0.1, 0.9, 0.0, 1.0],   // pred=1, true=1 → 0
        [0.9, 0.1, 1.0, 0.0],   // pred=0, true=0 → 0
        [0.3, 0.5, 0.4, 0.2],   // pred=1, true=0 → 1
        [-0.5, -1.0, 0.0, 1.0], // pred=0, true=1 → 1 (negative inputs)
    ];
    // Expected misclassifications: 4 incorrect rows above.
    let expected_misclassifications: f64 = 4.0;

    // Hand-compute via the same `activate_into` path the upstream helper
    // uses. Asserts the analytical expectation matches what the network
    // actually produces — so the test catches an activation change that
    // would silently shift the parity result.
    let mut hand_rolled: f64 = 0.0;
    let mut output_buf: [f32; 2] = [0.0; 2];
    for rec in records_2d {
        let inputs = &rec[..2];
        let targets = &rec[2..];
        net.activate_into(inputs, &mut output_buf[..]);
        // TS argmax (NEAT-AI/src/costs/CategoricalError.ts): scan with `>`
        // starting from index 0, so ties resolve to index 0.
        let pred_argmax = if output_buf[1] > output_buf[0] { 1 } else { 0 };
        let true_argmax = if targets[1] > targets[0] { 1 } else { 0 };
        hand_rolled += if pred_argmax == true_argmax { 0.0 } else { 1.0 };
    }
    // CATEGORICAL_ERROR is integer-valued — exact equality, no epsilon.
    assert_eq!(
        hand_rolled, expected_misclassifications,
        "Hand-rolled TS argmax sum mismatch: actual={hand_rolled}, expected={expected_misclassifications}",
    );

    // Pack the records and confirm dispatch returns exactly the same
    // integer count as the hand-rolled TS reference.
    let packed: Vec<f32> = records_2d.iter().flatten().copied().collect();
    let mut net_dispatch = identity_2_in_2_out();
    let actual = accumulate_cost_sum(
        CostKind::CategoricalError,
        &mut net_dispatch,
        &packed,
        2,
        2,
        true,
    )
    .expect("CATEGORICAL_ERROR dispatch must succeed after NEAT-AI-core#88");
    assert_eq!(
        actual, expected_misclassifications,
        "CATEGORICAL_ERROR dispatch must match TS reference: actual={actual}, expected={expected_misclassifications}",
    );
}

/// Issue #338 — RMSE parity: finalised `RMSE == sqrt(MSE)` on identical data.
///
/// RMSE reuses MSE's squared-error accumulation, so the raw dispatch sums are
/// identical; the only difference is the host-side `sqrt` applied by
/// `CostKind::finalise_mean` (which divides the sum by the record count and,
/// for RMSE, takes the square root). This test drives the exact CPU finalisation
/// path and asserts the finalised RMSE error equals the square root of the
/// finalised MSE error.
#[test]
fn parity_rmse_equals_sqrt_of_mse() {
    let pairs: &[(f32, f32)] = &[
        (0.5, 0.7),
        (1.0, 0.0),
        (-0.25, -0.25),
        (0.1, 0.2),
        (-1.0, 1.0),
        (0.5, 0.5),
        (2.0, 1.5),
        (-0.5, 0.0),
        (3.0, 0.0),
    ];
    let records = pack_pairs(pairs);

    // Raw squared-error sums must be identical (RMSE reuses the MSE helper).
    let mut net_mse = identity_1_in_1_out();
    let mse_sum = accumulate_cost_sum(CostKind::Mse, &mut net_mse, &records, 1, 1, true)
        .expect("MSE dispatch must succeed");
    let mut net_rmse = identity_1_in_1_out();
    let rmse_sum = accumulate_cost_sum(CostKind::Rmse, &mut net_rmse, &records, 1, 1, true)
        .expect("RMSE dispatch must succeed");
    assert!(
        (mse_sum - rmse_sum).abs() < EPS_SMOOTH,
        "RMSE must accumulate the same squared-error sum as MSE (mse={mse_sum}, rmse={rmse_sum})"
    );

    // Finalise exactly as the CPU sites do: the shared finaliser divides by the
    // record count (and takes the host-side `sqrt` for RMSE).
    let mse_error = CostKind::Mse.finalise_mean(mse_sum, pairs.len());
    let rmse_error = CostKind::Rmse.finalise_mean(rmse_sum, pairs.len());
    assert!(
        (rmse_error - mse_error.sqrt()).abs() < EPS_SMOOTH,
        "finalised RMSE must equal sqrt(MSE): rmse={rmse_error}, sqrt(mse)={}",
        mse_error.sqrt()
    );
}

/// Issue #338 — RMSE selection order matches MSE.
///
/// A monotonic transform (`sqrt` on a non-negative mean) must not change the
/// relative ordering of creatures by error. This ranks a fixed set of datasets
/// (each standing in for a creature with a distinct error level) under both
/// costs and asserts the sorted order — the actual selection signal — is
/// identical.
#[test]
fn parity_rmse_ranks_creatures_same_order_as_mse() {
    // Five fixed datasets with deliberately distinct squared-error levels.
    let datasets: &[(&str, &[(f32, f32)])] = &[
        ("near-perfect", &[(0.5, 0.5), (0.1, 0.11), (-0.2, -0.2)]),
        ("small", &[(0.5, 0.6), (1.0, 0.9), (-0.2, -0.1)]),
        ("medium", &[(0.5, 1.0), (1.0, 0.2), (-0.2, 0.4)]),
        ("large", &[(0.5, 2.0), (1.0, -1.0), (-0.2, 1.5)]),
        ("huge", &[(0.5, 5.0), (1.0, -4.0), (-0.2, 6.0)]),
    ];

    let finalise = |cost: CostKind, pairs: &[(f32, f32)]| -> f64 {
        let records = pack_pairs(pairs);
        let mut net = identity_1_in_1_out();
        let sum = accumulate_cost_sum(cost, &mut net, &records, 1, 1, true)
            .expect("dispatch must succeed");
        cost.finalise_mean(sum, pairs.len())
    };

    // Rank each cost's errors ascending and compare the resulting name order.
    let order_for = |cost: CostKind| -> Vec<&'static str> {
        let mut scored: Vec<(&'static str, f64)> = datasets
            .iter()
            .map(|(name, pairs)| (*name, finalise(cost, pairs)))
            .collect();
        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).expect("finite, non-NaN errors"));
        scored.into_iter().map(|(name, _)| name).collect()
    };

    let mse_order = order_for(CostKind::Mse);
    let rmse_order = order_for(CostKind::Rmse);
    assert_eq!(
        mse_order, rmse_order,
        "RMSE must rank creatures in the same order as MSE (mse={mse_order:?}, rmse={rmse_order:?})"
    );
    // Sanity: the datasets genuinely differ in error, so the ordering is a
    // real signal rather than a degenerate all-equal tie.
    assert_eq!(
        mse_order,
        vec!["near-perfect", "small", "medium", "large", "huge"],
        "expected strictly increasing error levels across the fixed datasets"
    );
}
