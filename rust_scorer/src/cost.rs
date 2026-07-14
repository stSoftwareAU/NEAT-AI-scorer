//! Cost-function selector for `rust_scorer` (Issues #120, #121).
//!
//! Adds a `--cost <NAME>` CLI flag that accepts the seven NEAT-AI built-in
//! cost names exactly as they appear in the TypeScript `BUILT_IN_COST_NAMES`
//! tuple (see `NEAT-AI/src/Costs.ts`). The flag defaults to `MSE`, which
//! preserves the current scoring behaviour. Issue #121 wires the selector
//! through to the per-chunk dispatch helper [`accumulate_cost_sum`] so the
//! fused/streaming CPU paths in `stream_score.rs` and `multi_score.rs`
//! actually compute the requested loss.
//!
//! KISS: there is **no** environment-variable override. Unknown values are
//! rejected at the clap layer with a non-zero exit and a stderr message
//! listing the supported set.

use clap::ValueEnum;
use neat_core::network::CompiledNetwork;

/// Typed error returned by [`CostKind::from_cli`] when a raw CLI value does not
/// match one of the built-in cost names (Issue #289, C-GOOD-ERR).
///
/// Replaces the previous `Result<_, String>` contract so callers can react to
/// the failure programmatically and compose it into their own
/// `std::error::Error` chains. Hand-rolls `Display`/`Error` following the
/// existing [`crate::gpu::GpuInitError`] pattern; the `Display` text is
/// preserved byte-for-byte from the old `format!(...)` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidCostName {
    /// The rejected raw CLI value.
    pub value: String,
}

impl std::fmt::Display for InvalidCostName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Invalid cost '{}': expected one of {}",
            self.value,
            supported_list(),
        )
    }
}

impl std::error::Error for InvalidCostName {}

/// Built-in NEAT-AI cost function selector.
///
/// The rendered names must match the TypeScript `BUILT_IN_COST_NAMES`
/// strings exactly so callers can pass `costName` through unchanged from
/// the upstream config (`NeatOptions.costName`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum CostKind {
    /// Mean Squared Error — current (and default) scoring path.
    #[default]
    #[value(name = "MSE")]
    Mse,
    /// Root Mean Squared Error (Issue #339). Reuses the MSE squared-error
    /// sum unchanged and differs only in a host-side `sqrt` at finalisation
    /// (see [`CostKind::finalise_mean`]), so it runs on the GPU MSE kernel at
    /// full MSE speed with no new kernel.
    #[value(name = "RMSE")]
    Rmse,
    /// Mean Absolute Error.
    #[value(name = "MAE")]
    Mae,
    /// Mean Absolute Percentage Error.
    #[value(name = "MAPE")]
    Mape,
    /// Mean Squared Logarithmic Error.
    #[value(name = "MSLE")]
    Msle,
    /// Hinge loss (margin classifiers).
    #[value(name = "HINGE")]
    Hinge,
    /// Cross-entropy (probabilistic classifiers).
    #[value(name = "CROSS_ENTROPY")]
    CrossEntropy,
    /// Categorical error (multi-class top-1 mismatch).
    #[value(name = "CATEGORICAL_ERROR")]
    CategoricalError,
}

impl CostKind {
    /// Stable serialised label as a `&'static str`. Matches the TypeScript
    /// `BUILT_IN_COST_NAMES` strings exactly. Used both by tests/benches and
    /// by Issue #121's `costName` JSON field on `ScoreResult`.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_scorer::cost::CostKind;
    /// assert_eq!(CostKind::Mse.as_str(), "MSE");
    /// assert_eq!(CostKind::CrossEntropy.as_str(), "CROSS_ENTROPY");
    /// ```
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mse => "MSE",
            Self::Rmse => "RMSE",
            Self::Mae => "MAE",
            Self::Mape => "MAPE",
            Self::Msle => "MSLE",
            Self::Hinge => "HINGE",
            Self::CrossEntropy => "CROSS_ENTROPY",
            Self::CategoricalError => "CATEGORICAL_ERROR",
        }
    }

    /// Validate a raw CLI string and return the matching [`CostKind`].
    ///
    /// Centralises validation so unit tests can exercise the accept/reject
    /// logic without going through clap's argv parser. The error message
    /// lists every supported name in the TypeScript order to match the
    /// `--help` output.
    ///
    /// Returns `Err` (with a message listing the supported names) when `raw`
    /// does not exactly match one of the `BUILT_IN_COST_NAMES` strings.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_scorer::cost::CostKind;
    /// assert_eq!(CostKind::from_cli("MSE").unwrap(), CostKind::Mse);
    /// assert!(CostKind::from_cli("FOO").is_err());
    /// ```
    // Consumed by unit tests and downstream callers (`NeatOptions.costName`
    // pass-through, dispatch landing in #119-3); the bin target itself
    // relies on clap, not this helper.
    #[allow(dead_code)]
    pub fn from_cli(raw: &str) -> Result<Self, InvalidCostName> {
        // Exact-match parsing — the TypeScript names are upper-case, so we
        // do not normalise case. This keeps the CLI contract aligned with
        // what `NeatOptions.costName` will pass through.
        for variant in Self::value_variants() {
            if variant.as_str() == raw {
                return Ok(*variant);
            }
        }
        Err(InvalidCostName {
            value: raw.to_string(),
        })
    }
}

impl CostKind {
    /// Issue #121 / #339: which costs the GPU `forward_mse_batched` kernel can
    /// service. [`CostKind::Mse`] and [`CostKind::Rmse`] today — RMSE reuses
    /// the MSE squared-error sum unchanged and only adds a host-side `sqrt` at
    /// finalisation (no new kernel, see [`CostKind::finalise_mean`]). Every
    /// other cost forces a CPU fallback (or a hard error under `--gpu on`).
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_scorer::cost::CostKind;
    /// assert!(CostKind::Mse.gpu_supported());
    /// assert!(CostKind::Rmse.gpu_supported());
    /// assert!(!CostKind::Mae.gpu_supported());
    /// ```
    pub const fn gpu_supported(self) -> bool {
        matches!(self, Self::Mse | Self::Rmse)
    }

    /// Finalise an accumulated per-record error sum into the mean loss the
    /// scorer reports (Issue #339).
    ///
    /// Every scoring site accumulates the un-normalised loss sum over records
    /// (see [`accumulate_cost_sum`]) and then divides by the record count.
    /// [`CostKind::Rmse`] additionally takes the square root of that mean — it
    /// reuses the MSE squared-error sum unchanged and differs only in this
    /// host-side finalisation. Centralising the division (and the optional
    /// `sqrt`) keeps the three finalisation sites — the single-creature path in
    /// `main.rs` and the CPU and GPU creature-directory paths in
    /// `multi_score.rs` — in lock-step so RMSE cannot silently report MSE-scale
    /// numbers on one path.
    ///
    /// `record_count` must be non-zero; every call site rejects a zero-record
    /// creature before finalising, so this helper does not re-guard the divide.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_scorer::cost::CostKind;
    /// // MSE reports the plain mean; RMSE reports its square root.
    /// assert_eq!(CostKind::Mse.finalise_mean(8.0, 2), 4.0);
    /// assert_eq!(CostKind::Rmse.finalise_mean(8.0, 2), 2.0);
    /// ```
    pub fn finalise_mean(self, error_sum: f64, record_count: usize) -> f64 {
        let mean = error_sum / record_count as f64;
        match self {
            Self::Rmse => mean.sqrt(),
            _ => mean,
        }
    }
}

/// Issue #121 — per-chunk cost dispatch.
///
/// Routes a packed `[inputs..., targets...]` chunk to the matching
/// `*_sum_batch_packed` helper in `neat_core::loss`. The fused streaming
/// path and the multi-creature directory path both call into this single
/// dispatch site so adding a future cost is a one-line change here.
///
/// Returns `Ok(sum)` — the un-normalised loss sum over every record in
/// `chunk`. As of Issue #134 every built-in cost name dispatches; the
/// `Result` return type is retained so future costs that need to surface
/// a setup error (e.g. a kernel-only cost on the CPU path) can do so
/// without churning every call site.
///
/// Returns `Err` when `chunk`'s float length is not a whole multiple of the
/// `input_size + num_outputs` record stride (a malformed packed chunk).
///
/// # Examples
///
/// ```
/// use rust_scorer::cost::{CostKind, accumulate_cost_sum};
/// use neat_core::creature::{CreatureExport, NeuronExport, SynapseExport, compile_creature};
///
/// // A trivial forward-only creature: one input wired straight to one output.
/// let creature = CreatureExport {
///     input: 1,
///     output: 1,
///     neurons: vec![NeuronExport {
///         neuron_type: "output".to_string(),
///         uuid: "output-0".to_string(),
///         bias: 0.0,
///         squash: Some("IDENTITY".to_string()),
///     }],
///     synapses: vec![SynapseExport {
///         from_uuid: "input-0".to_string(),
///         to_uuid: "output-0".to_string(),
///         weight: 1.0,
///         synapse_type: None,
///     }],
///     semantic_version: Some("4.0.0".to_string()),
///     forward_only: true,
/// };
/// let mut network = compile_creature(&creature).unwrap();
///
/// // One packed record `[input, target]`; MSE sum is finite and non-negative.
/// let sum = accumulate_cost_sum(CostKind::Mse, &mut network, &[0.5, 0.5], 1, 1, true).unwrap();
/// assert!(sum.is_finite() && sum >= 0.0);
///
/// // A chunk length that is not a whole multiple of the 2-value stride errors.
/// assert!(accumulate_cost_sum(CostKind::Mse, &mut network, &[0.5], 1, 1, true).is_err());
/// ```
pub fn accumulate_cost_sum(
    kind: CostKind,
    network: &mut CompiledNetwork,
    chunk: &[f32],
    input_size: usize,
    num_outputs: usize,
    forward_only: bool,
) -> Result<f64, String> {
    use neat_core::loss;
    // Issue #200: reject malformed chunks whose float length is not a whole
    // number of packed records. The upstream `*_sum_batch_packed` helpers
    // silently truncate a trailing partial record, so a content-dependent
    // size mismatch would otherwise pass unnoticed. Surfacing a structured
    // `Err` here lets the parallel per-chunk dispatch propagate the failure
    // instead of relying on a panic (`.expect`) when such a case appears.
    let values_per_record = input_size + num_outputs;
    if values_per_record > 0 && chunk.len() % values_per_record != 0 {
        return Err(format!(
            "Malformed record chunk: {} float(s) is not a whole multiple of the {}-value record stride",
            chunk.len(),
            values_per_record
        ));
    }
    Ok(match kind {
        // Issue #339: RMSE reuses the MSE squared-error sum unchanged — the
        // only difference is the host-side `sqrt` applied in
        // [`CostKind::finalise_mean`], never here in the per-chunk sum.
        CostKind::Mse | CostKind::Rmse => {
            loss::mse_sum_batch_packed(network, chunk, input_size, num_outputs, forward_only)
        }
        CostKind::Mae => {
            loss::mae_sum_batch_packed(network, chunk, input_size, num_outputs, forward_only)
        }
        CostKind::Mape => {
            loss::mape_sum_batch_packed(network, chunk, input_size, num_outputs, forward_only)
        }
        CostKind::Msle => {
            loss::msle_sum_batch_packed(network, chunk, input_size, num_outputs, forward_only)
        }
        CostKind::Hinge => {
            loss::hinge_sum_batch_packed(network, chunk, input_size, num_outputs, forward_only)
        }
        CostKind::CrossEntropy => loss::cross_entropy_sum_batch_packed(
            network,
            chunk,
            input_size,
            num_outputs,
            forward_only,
        ),
        CostKind::CategoricalError => loss::categorical_error_sum_batch_packed(
            network,
            chunk,
            input_size,
            num_outputs,
            forward_only,
        ),
    })
}

/// Comma-separated list of supported cost names in TypeScript order
/// (matches `BUILT_IN_COST_NAMES`). Used to build the error message
/// returned by [`CostKind::from_cli`] when a value is rejected.
#[allow(dead_code)]
fn supported_list() -> String {
    CostKind::value_variants()
        .iter()
        .map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    /// Every TypeScript `BUILT_IN_COST_NAMES` entry must parse via `from_cli`.
    /// This pins the CLI contract against the upstream cost-name set.
    #[test]
    fn from_cli_accepts_every_built_in_cost_name() {
        for name in [
            "MSE",
            "MAE",
            "MAPE",
            "MSLE",
            "HINGE",
            "CROSS_ENTROPY",
            "CATEGORICAL_ERROR",
        ] {
            let parsed = CostKind::from_cli(name)
                .unwrap_or_else(|e| panic!("expected '{name}' to parse, got error: {e}"));
            assert_eq!(parsed.as_str(), name);
        }
    }

    /// Unknown cost names must be rejected with a helpful message listing
    /// the supported set.
    #[test]
    fn from_cli_rejects_unknown_cost_name() {
        // Issue #289: `from_cli` now returns the typed `InvalidCostName`; assert
        // on its `Display` text so the historical message contract is preserved.
        let err = CostKind::from_cli("FOO")
            .expect_err("FOO must be rejected")
            .to_string();
        assert!(
            err.contains("FOO"),
            "error must echo the bad value, got: {err}"
        );
        for name in [
            "MSE",
            "MAE",
            "MAPE",
            "MSLE",
            "HINGE",
            "CROSS_ENTROPY",
            "CATEGORICAL_ERROR",
        ] {
            assert!(
                err.contains(name),
                "error must list supported cost '{name}', got: {err}"
            );
        }
    }

    /// Case-mismatched names must be rejected — the TS `BUILT_IN_COST_NAMES`
    /// strings are upper-case and the CLI contract is exact.
    #[test]
    fn from_cli_rejects_case_mismatch() {
        assert!(CostKind::from_cli("mse").is_err());
        assert!(CostKind::from_cli("Cross_Entropy").is_err());
    }

    /// Empty input is not a valid cost name.
    #[test]
    fn from_cli_rejects_empty_string() {
        assert!(CostKind::from_cli("").is_err());
    }

    /// Issue #289 (C-GOOD-ERR): `from_cli` returns a typed `InvalidCostName`
    /// that exposes the rejected value as a field and composes into a caller's
    /// `Box<dyn std::error::Error>` chain.
    #[test]
    fn from_cli_returns_typed_invalid_cost_name() {
        let err = CostKind::from_cli("FOO").expect_err("FOO must be rejected");
        assert_eq!(
            err,
            InvalidCostName {
                value: "FOO".to_string()
            }
        );

        fn caller() -> Result<CostKind, Box<dyn std::error::Error>> {
            Ok(CostKind::from_cli("BAR")?)
        }
        let boxed = caller().unwrap_err();
        assert!(boxed.to_string().contains("BAR"));
        assert!(boxed.downcast_ref::<InvalidCostName>().is_some());
    }

    /// The default must be MSE so the historical scoring behaviour is
    /// preserved when callers omit `--cost`.
    #[test]
    fn default_is_mse() {
        assert_eq!(CostKind::default(), CostKind::Mse);
        assert_eq!(CostKind::default().as_str(), "MSE");
    }

    /// Every variant must round-trip through clap's `ValueEnum` using the
    /// rendered name (case-sensitive — `ignore_case = false`).
    #[test]
    fn clap_value_enum_round_trips_every_variant() {
        for variant in CostKind::value_variants() {
            let possible = variant
                .to_possible_value()
                .expect("every variant must have a possible value");
            let name = possible.get_name();
            let parsed = CostKind::from_str(name, false)
                .unwrap_or_else(|e| panic!("clap could not parse '{name}': {e}"));
            assert_eq!(parsed, *variant);
        }
    }

    /// Issue #121/#339: MSE and RMSE are GPU-supported (RMSE reuses the MSE
    /// kernel with a host-side `sqrt`); every other cost must run on CPU.
    /// Locks the predicate so a regression that drops RMSE back to CPU — or
    /// that accidentally makes another cost GPU-eligible — fails immediately.
    #[test]
    fn gpu_supported_only_for_mse_and_rmse() {
        assert!(CostKind::Mse.gpu_supported());
        assert!(
            CostKind::Rmse.gpu_supported(),
            "RMSE must be GPU-supported: it reuses the MSE kernel (Issue #339)"
        );
        for v in [
            CostKind::Mae,
            CostKind::Mape,
            CostKind::Msle,
            CostKind::Hinge,
            CostKind::CrossEntropy,
            CostKind::CategoricalError,
        ] {
            assert!(
                !v.gpu_supported(),
                "{} must not be GPU-supported until a new kernel ships",
                v.as_str()
            );
        }
    }

    /// Issue #339: `finalise_mean` divides the accumulated error sum by the
    /// record count and applies a `sqrt` for — and only for — RMSE. This is
    /// the host-side finalisation that turns the shared MSE squared-error sum
    /// into an RMSE score, so it must be exact for RMSE and a plain mean for
    /// every other cost.
    #[test]
    fn finalise_mean_applies_sqrt_only_for_rmse() {
        // sum=8 over 2 records → mean 4.0; RMSE = sqrt(4) = 2.0.
        assert_eq!(CostKind::Mse.finalise_mean(8.0, 2), 4.0);
        assert_eq!(CostKind::Rmse.finalise_mean(8.0, 2), 2.0);
        // Every non-RMSE cost reports the plain mean (no sqrt).
        for v in [
            CostKind::Mse,
            CostKind::Mae,
            CostKind::Mape,
            CostKind::Msle,
            CostKind::Hinge,
            CostKind::CrossEntropy,
            CostKind::CategoricalError,
        ] {
            assert_eq!(
                v.finalise_mean(9.0, 3),
                3.0,
                "{} must report the plain mean without a sqrt",
                v.as_str()
            );
        }
        // RMSE is exactly the sqrt of the MSE finalisation over the same sum.
        let sum = 12.5_f64;
        let count = 5;
        let mse = CostKind::Mse.finalise_mean(sum, count);
        let rmse = CostKind::Rmse.finalise_mean(sum, count);
        assert!((rmse - mse.sqrt()).abs() < 1e-12);
    }

    /// Issue #339: RMSE must reuse the MSE squared-error sum unchanged — the
    /// `sqrt` lives only in `finalise_mean`, never in the per-chunk dispatch.
    /// So `accumulate_cost_sum` for RMSE must equal the MSE sum bit-for-bit.
    #[test]
    fn accumulate_cost_sum_rmse_matches_mse_sum() {
        use neat_core::creature::{compile_creature, parse_creature_json};
        let json = r#"{"input":1,"output":1,"forwardOnly":true,"neurons":[
            {"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}
        ],"synapses":[
            {"fromUUID":"input-0","toUUID":"output-0","weight":1.0}
        ]}"#;
        let creature = parse_creature_json(json).unwrap();
        // |diff| = 1.5 per record so the squared-error sum is non-trivial.
        let chunk = vec![2.0_f32, 0.5_f32, -1.0_f32, 0.5_f32];

        let mut net_mse = compile_creature(&creature).unwrap();
        let mse = accumulate_cost_sum(CostKind::Mse, &mut net_mse, &chunk, 1, 1, true).unwrap();
        let mut net_rmse = compile_creature(&creature).unwrap();
        let rmse = accumulate_cost_sum(CostKind::Rmse, &mut net_rmse, &chunk, 1, 1, true).unwrap();
        assert_eq!(
            mse, rmse,
            "RMSE must reuse the MSE squared-error sum unchanged (mse={mse}, rmse={rmse})"
        );
        assert!(
            mse > 0.0,
            "expected a positive squared-error sum, got {mse}"
        );
    }

    /// Issue #339: RMSE parses via `from_cli` and renders back as `RMSE`.
    /// RMSE is not (yet) an upstream `BUILT_IN_COST_NAMES` value — that sync
    /// is Issue #340 — so it lives alongside the seven built-ins rather than
    /// inside the "every built-in" contract tests.
    #[test]
    fn from_cli_accepts_rmse() {
        assert_eq!(CostKind::from_cli("RMSE").unwrap(), CostKind::Rmse);
        assert_eq!(CostKind::Rmse.as_str(), "RMSE");
        // Case-sensitive, like the other names.
        assert!(CostKind::from_cli("rmse").is_err());
    }

    /// Issue #134: now that `categorical_error_sum_batch_packed` has
    /// landed in `neat-core` (NEAT-AI-core#88), `CATEGORICAL_ERROR`
    /// dispatches through `accumulate_cost_sum` and must return the
    /// same value as the direct helper call. Locks the dispatch wiring
    /// so a future regression to the old "blocked" branch fails loudly.
    #[test]
    fn accumulate_cost_sum_categorical_error_matches_direct_helper() {
        use neat_core::creature::{compile_creature, parse_creature_json};
        use neat_core::loss::categorical_error_sum_batch_packed;
        // 1-input, 2-output IDENTITY creature so argmax over the outputs
        // is the index of the larger input — exercises a real
        // misclassification path (single-output argmax is degenerate).
        let json = r#"{"input":2,"output":2,"forwardOnly":true,"neurons":[
            {"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"},
            {"type":"output","uuid":"output-1","bias":0.0,"squash":"IDENTITY"}
        ],"synapses":[
            {"fromUUID":"input-0","toUUID":"output-0","weight":1.0},
            {"fromUUID":"input-1","toUUID":"output-1","weight":1.0}
        ]}"#;
        let creature = parse_creature_json(json).unwrap();
        // [in0, in1, t0, t1] per record. 2 wrong, 1 correct.
        let chunk = vec![
            0.8_f32, 0.2, 1.0, 0.0, // pred=0, true=0 → 0
            0.6, 0.4, 0.0, 1.0, // pred=0, true=1 → 1
            0.4, 0.6, 1.0, 0.0, // pred=1, true=0 → 1
        ];

        let mut net_direct = compile_creature(&creature).unwrap();
        let direct = categorical_error_sum_batch_packed(&mut net_direct, &chunk, 2, 2, true);
        let mut net_dispatch = compile_creature(&creature).unwrap();
        let via_dispatch = accumulate_cost_sum(
            CostKind::CategoricalError,
            &mut net_dispatch,
            &chunk,
            2,
            2,
            true,
        )
        .expect("categorical_error must dispatch after NEAT-AI-core#88");
        // CATEGORICAL_ERROR is integer-valued — exact equality.
        assert_eq!(
            direct, via_dispatch,
            "dispatch must match direct categorical_error_sum_batch_packed (direct={direct}, dispatch={via_dispatch})"
        );
        assert_eq!(
            via_dispatch, 2.0,
            "expected 2 misclassifications, got {via_dispatch}"
        );
    }

    /// Issue #200: a malformed chunk whose float length is not a whole
    /// multiple of the record stride must return a structured `Err` rather
    /// than silently truncating or panicking. This is the per-chunk error
    /// path that the parallel dispatch loops now propagate via `?`/`collect`
    /// instead of `.expect`.
    #[test]
    fn accumulate_cost_sum_rejects_malformed_chunk() {
        use neat_core::creature::{compile_creature, parse_creature_json};
        let json = r#"{"input":1,"output":1,"forwardOnly":true,"neurons":[
            {"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}
        ],"synapses":[
            {"fromUUID":"input-0","toUUID":"output-0","weight":1.0}
        ]}"#;
        let creature = parse_creature_json(json).unwrap();
        let mut net = compile_creature(&creature).unwrap();
        // Stride is 2 (1 input + 1 output); 3 floats is one-and-a-half records.
        let chunk = vec![0.5_f32, 0.5_f32, 0.25_f32];
        let err = accumulate_cost_sum(CostKind::Mse, &mut net, &chunk, 1, 1, true)
            .expect_err("misaligned chunk must be rejected");
        assert!(
            err.contains("Malformed record chunk"),
            "error must name the malformed chunk, got: {err}"
        );
    }

    /// Issue #200: an error returned by `accumulate_cost_sum` inside a Rayon
    /// parallel closure must surface as a clean `Err` (via `collect` into a
    /// `Result`) instead of aborting the process with a panic. This locks the
    /// propagation mechanism used by the per-chunk dispatch in `multi_score`
    /// and `stream_score`.
    #[test]
    fn accumulate_cost_sum_error_propagates_through_rayon() {
        use neat_core::creature::{compile_creature, parse_creature_json};
        use rayon::prelude::*;
        let json = r#"{"input":1,"output":1,"forwardOnly":true,"neurons":[
            {"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}
        ],"synapses":[
            {"fromUUID":"input-0","toUUID":"output-0","weight":1.0}
        ]}"#;
        let creature = parse_creature_json(json).unwrap();
        let mut nets = vec![
            compile_creature(&creature).unwrap(),
            compile_creature(&creature).unwrap(),
        ];
        // Worker 0 gets a valid aligned slice; worker 1 gets a malformed one.
        let slices: Vec<&[f32]> = vec![&[0.5_f32, 0.5_f32], &[0.25_f32]];
        let collected: Result<Vec<f64>, String> = nets
            .par_iter_mut()
            .zip(slices)
            .map(|(net, slice)| accumulate_cost_sum(CostKind::Mse, net, slice, 1, 1, true))
            .collect();
        let err = collected.expect_err("malformed slice must propagate as Err, not panic");
        assert!(
            err.contains("Malformed record chunk"),
            "propagated error must name the malformed chunk, got: {err}"
        );
    }

    /// Issue #121: dispatching `MSE` via `accumulate_cost_sum` must match
    /// what `mse_sum_batch_packed` would have produced from the previous
    /// hard-coded call site, so the existing MSE-only behaviour is
    /// numerically preserved.
    #[test]
    fn accumulate_cost_sum_mse_matches_direct_helper() {
        use neat_core::creature::{compile_creature, parse_creature_json};
        use neat_core::loss::mse_sum_batch_packed;
        let json = r#"{"input":1,"output":1,"forwardOnly":true,"neurons":[
            {"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}
        ],"synapses":[
            {"fromUUID":"input-0","toUUID":"output-0","weight":1.0}
        ]}"#;
        let creature = parse_creature_json(json).unwrap();
        // Two records, [inputs, targets] packed: predict input == target after
        // identity activation, so MSE here is 0.
        let chunk = vec![0.5_f32, 0.5_f32, -0.25_f32, -0.25_f32];

        let mut net_a = compile_creature(&creature).unwrap();
        let direct = mse_sum_batch_packed(&mut net_a, &chunk, 1, 1, true);
        let mut net_b = compile_creature(&creature).unwrap();
        let via_dispatch =
            accumulate_cost_sum(CostKind::Mse, &mut net_b, &chunk, 1, 1, true).unwrap();
        assert!(
            (direct - via_dispatch).abs() < 1e-12,
            "dispatch must match direct mse_sum_batch_packed (direct={direct}, dispatch={via_dispatch})"
        );
    }

    /// Issue #121: dispatching `MAE` must call the matching helper. The
    /// identity creature plus a deliberately mismatched target produces a
    /// finite, positive sum that MSE alone cannot reproduce — so this also
    /// asserts that MSE and MAE do not collapse to the same code path.
    #[test]
    fn accumulate_cost_sum_mae_diverges_from_mse() {
        use neat_core::creature::{compile_creature, parse_creature_json};
        let json = r#"{"input":1,"output":1,"forwardOnly":true,"neurons":[
            {"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}
        ],"synapses":[
            {"fromUUID":"input-0","toUUID":"output-0","weight":1.0}
        ]}"#;
        let creature = parse_creature_json(json).unwrap();
        // Inputs of 2.0 (predicted 2.0 after identity), target 0.5 → |diff|=1.5,
        // diff^2=2.25 — MAE and MSE must differ.
        let chunk = vec![2.0_f32, 0.5_f32];

        let mut net_mse = compile_creature(&creature).unwrap();
        let mse = accumulate_cost_sum(CostKind::Mse, &mut net_mse, &chunk, 1, 1, true).unwrap();
        let mut net_mae = compile_creature(&creature).unwrap();
        let mae = accumulate_cost_sum(CostKind::Mae, &mut net_mae, &chunk, 1, 1, true).unwrap();
        assert!(
            mse > 0.0 && mae > 0.0,
            "both should be positive: mse={mse}, mae={mae}"
        );
        assert!(
            (mse - mae).abs() > 1e-6,
            "MSE and MAE must take different code paths (mse={mse}, mae={mae})"
        );
        // MAE(0.5)=1.5, MSE(0.5)=2.25 — confirm shape rather than exact equality
        // (the helpers may normalise by num_outputs internally).
        assert!(mse > mae, "for |diff|=1.5 > 1, MSE > MAE");
    }

    /// Issue #120 explicitly forbids an environment-variable override.
    /// Encode that contract by asserting the env var is ignored: the
    /// `from_cli` helper takes only the raw CLI value and never reads
    /// process state, so setting `NEAT_SCORER_COST` cannot influence
    /// validation.
    #[test]
    fn from_cli_ignores_env_var_override() {
        // Safety: tests run in-process; setting an env var is benign here
        // because `from_cli` never reads `std::env`.
        // SAFETY: single-threaded test scope, no other reader observes the var.
        unsafe {
            std::env::set_var("NEAT_SCORER_COST", "MAE");
        }
        // The helper still rejects unknown values regardless of env state.
        assert!(CostKind::from_cli("FOO").is_err());
        // And it still returns exactly what the CLI string says.
        assert_eq!(CostKind::from_cli("MSE").unwrap(), CostKind::Mse);
        // SAFETY: single-threaded test scope, no other reader observes the var.
        unsafe {
            std::env::remove_var("NEAT_SCORER_COST");
        }
    }
}
