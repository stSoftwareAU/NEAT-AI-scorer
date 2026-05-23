//! Cost-function selector for `rust_scorer` (Issue #120).
//!
//! Adds a `--cost <NAME>` CLI flag that accepts the seven NEAT-AI built-in
//! cost names exactly as they appear in the TypeScript `BUILT_IN_COST_NAMES`
//! tuple (see `NEAT-AI/src/Costs.ts`). The flag defaults to `MSE`, which
//! preserves the current scoring behaviour — actual dispatch onto the new
//! cost kinds lands in the follow-up issue (#119-3); this change is the
//! foundational plumbing only.
//!
//! KISS: there is **no** environment-variable override. Unknown values are
//! rejected at the clap layer with a non-zero exit and a stderr message
//! listing the supported set.

use clap::ValueEnum;

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
    /// `BUILT_IN_COST_NAMES` strings exactly.
    // Consumed by tests, benches, and the follow-up dispatch wiring in
    // #119-3. Suppressed for the `bin "rust_scorer"` compile where the
    // CLI binary itself only reads the variant, not its rendered label.
    #[allow(dead_code)]
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
