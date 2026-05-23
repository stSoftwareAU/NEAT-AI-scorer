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
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mse => "MSE",
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
    // Consumed by unit tests and downstream callers (`NeatOptions.costName`
    // pass-through, dispatch landing in #119-3); the bin target itself
    // relies on clap, not this helper.
    #[allow(dead_code)]
    pub fn from_cli(raw: &str) -> Result<Self, String> {
        // Exact-match parsing — the TypeScript names are upper-case, so we
        // do not normalise case. This keeps the CLI contract aligned with
        // what `NeatOptions.costName` will pass through.
        for variant in Self::value_variants() {
            if variant.as_str() == raw {
                return Ok(*variant);
            }
        }
        Err(format!(
            "Invalid cost '{raw}': expected one of {}",
            supported_list(),
        ))
    }
}

impl CostKind {
    /// Issue #121: which costs the GPU `forward_mse_batched` kernel can
    /// service. Only [`CostKind::Mse`] today — every other cost forces a
    /// CPU fallback (or a hard error under `--gpu on`).
    pub const fn gpu_supported(self) -> bool {
        matches!(self, Self::Mse)
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
/// `chunk` — or `Err(message)` when the requested cost is not yet wired
/// (currently [`CostKind::CategoricalError`], which depends on
/// `stSoftwareAU/NEAT-AI-core#88` to land first).
pub fn accumulate_cost_sum(
    kind: CostKind,
    network: &mut CompiledNetwork,
    chunk: &[f32],
    input_size: usize,
    num_outputs: usize,
    forward_only: bool,
) -> Result<f64, String> {
    use neat_core::loss;
    Ok(match kind {
        CostKind::Mse => {
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
        CostKind::CategoricalError => {
            return Err(
                "CATEGORICAL_ERROR dispatch is blocked on stSoftwareAU/NEAT-AI-core#88 \
                 (categorical_error_sum_batch_packed) — rerun with a different --cost \
                 until that helper lands in neat-core"
                    .to_string(),
            );
        }
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
        let err = CostKind::from_cli("FOO").expect_err("FOO must be rejected");
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

    /// Issue #121: only MSE is GPU-supported today; every other cost must
    /// run on CPU. Locks the predicate so a future kernel that supports
    /// more costs has an obvious one-line update + test failure.
    #[test]
    fn gpu_supported_only_for_mse() {
        assert!(CostKind::Mse.gpu_supported());
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

    /// Issue #121: `CATEGORICAL_ERROR` is currently blocked on the missing
    /// `categorical_error_sum_batch_packed` upstream in NEAT-AI-core#88.
    /// Dispatching it must return a clear, actionable error string — not
    /// panic, not silently compute MSE. Locks that behaviour so once #88
    /// lands the helper can be swapped in without touching call sites.
    #[test]
    fn accumulate_cost_sum_categorical_error_is_blocked() {
        use neat_core::creature::{compile_creature, parse_creature_json};
        // Minimal forwardOnly identity creature.
        let json = r#"{"input":1,"output":1,"forwardOnly":true,"neurons":[
            {"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}
        ],"synapses":[
            {"fromUUID":"input-0","toUUID":"output-0","weight":1.0}
        ]}"#;
        let creature = parse_creature_json(json).unwrap();
        let mut net = compile_creature(&creature).unwrap();
        let chunk = vec![0.5_f32, 0.5_f32];
        let err = accumulate_cost_sum(CostKind::CategoricalError, &mut net, &chunk, 1, 1, true)
            .expect_err("categorical error must be blocked");
        assert!(
            err.contains("CATEGORICAL_ERROR") && err.contains("#88"),
            "error should mention CATEGORICAL_ERROR and #88, got: {err}"
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
