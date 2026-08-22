//! The observation-width guard — Issue #571.
//!
//! The `CreatureExport` top-level `input` / `output` integers are the fleet's
//! **observation width contract**: `input` is the observation count and
//! `output` the target count. `neurons` deliberately lists only *non-input*
//! neurons, so `input` **cannot be re-derived** from the neuron list. A creature
//! with `input < 1` or `output < 1` is therefore never accepted anywhere — not
//! on read, not on compile, not before a single training byte is opened. There
//! is no fallback and no default.
//!
//! This module is the one place that error is worded, so every scoring path
//! (single-creature, directory / GPU, fused streaming, the cost dispatcher and
//! the fixture loaders) reports the same string. The wording mirrors the
//! TypeScript reference (`CreatureValidate.ts`) and the `neat-core`
//! `InvalidInputCount` / `InvalidOutputCount` errors that
//! [NEAT-AI-core#550](https://github.com/stSoftwareAU/NEAT-AI-core/issues/550)
//! adds, so logs never show two divergent messages. The local guard stays even
//! after that release: it is the scorer's boundary and must fail loudly
//! regardless of the core version it is built against.

use neat_core::creature::CreatureExport;

/// A creature declared an observation width the scorer must never accept.
///
/// `Display` mirrors the TypeScript reference wording byte-for-byte:
/// `Must have at least one input neurons was: N` /
/// `Must have at least one output neurons was: N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatureWidthError {
    /// The top-level `input` count was below one.
    InvalidInputCount {
        /// The declared `input` value.
        found: usize,
    },
    /// The top-level `output` count was below one.
    InvalidOutputCount {
        /// The declared `output` value.
        found: usize,
    },
}

impl std::fmt::Display for CreatureWidthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInputCount { found } => {
                write!(f, "Must have at least one input neurons was: {found}")
            }
            Self::InvalidOutputCount { found } => {
                write!(f, "Must have at least one output neurons was: {found}")
            }
        }
    }
}

impl std::error::Error for CreatureWidthError {}

/// Reject an observation width with `input < 1` or `output < 1`.
///
/// `input` is checked first (matching the TypeScript reference order), so a
/// creature that fails both reports the input count.
///
/// # Examples
///
/// ```
/// use rust_scorer::creature_width::{CreatureWidthError, validate_observation_width};
///
/// assert_eq!(validate_observation_width(3, 1), Ok(()));
/// assert_eq!(
///     validate_observation_width(0, 1),
///     Err(CreatureWidthError::InvalidInputCount { found: 0 })
/// );
/// assert_eq!(
///     validate_observation_width(3, 0).unwrap_err().to_string(),
///     "Must have at least one output neurons was: 0"
/// );
/// ```
pub fn validate_observation_width(input: usize, output: usize) -> Result<(), CreatureWidthError> {
    if input < 1 {
        return Err(CreatureWidthError::InvalidInputCount { found: input });
    }
    if output < 1 {
        return Err(CreatureWidthError::InvalidOutputCount { found: output });
    }
    Ok(())
}

/// Reject a parsed creature whose top-level `input` / `output` count is below
/// one. Call this immediately after `parse_creature_json` and before compiling
/// or opening any training data.
///
/// # Examples
///
/// ```
/// use neat_core::creature::parse_creature_json;
/// use rust_scorer::creature_width::validate_creature_width;
///
/// let creature = parse_creature_json(
///     r#"{"input":1,"output":1,"forwardOnly":true,"neurons":[
///         {"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}],
///         "synapses":[{"fromUUID":"input-0","toUUID":"output-0","weight":1.0}]}"#,
/// )
/// .unwrap();
/// assert!(validate_creature_width(&creature).is_ok());
/// ```
pub fn validate_creature_width(creature: &CreatureExport) -> Result<(), CreatureWidthError> {
    validate_observation_width(creature.input, creature.output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use neat_core::creature::NeuronExport;

    #[test]
    fn zero_input_is_rejected_with_the_reference_wording() {
        let err = validate_observation_width(0, 1).unwrap_err();
        assert_eq!(err, CreatureWidthError::InvalidInputCount { found: 0 });
        assert_eq!(
            err.to_string(),
            "Must have at least one input neurons was: 0"
        );
    }

    #[test]
    fn zero_output_is_rejected_with_the_reference_wording() {
        let err = validate_observation_width(1, 0).unwrap_err();
        assert_eq!(err, CreatureWidthError::InvalidOutputCount { found: 0 });
        assert_eq!(
            err.to_string(),
            "Must have at least one output neurons was: 0"
        );
    }

    #[test]
    fn input_is_checked_before_output() {
        assert_eq!(
            validate_observation_width(0, 0),
            Err(CreatureWidthError::InvalidInputCount { found: 0 })
        );
    }

    #[test]
    fn positive_widths_pass() {
        assert_eq!(validate_observation_width(1, 1), Ok(()));
        assert_eq!(validate_observation_width(2511, 1), Ok(()));
    }

    #[test]
    fn creature_guard_reads_the_top_level_counts_not_the_neuron_list() {
        // `neurons` lists only non-input neurons: an export with a full output
        // neuron list but `input: 0` must still be rejected, because the
        // observation count cannot be re-derived from the neuron list. Built
        // as a struct literal (not parsed) so the test keeps exercising this
        // guard once `neat-core` itself rejects the JSON (NEAT-AI-core#550).
        let creature = CreatureExport {
            input: 0,
            output: 1,
            neurons: vec![NeuronExport {
                id: None,
                neuron_type: "output".to_string(),
                uuid: "output-0".to_string(),
                bias: 0.0,
                squash: Some("IDENTITY".to_string()),
            }],
            synapses: vec![],
            semantic_version: None,
            forward_only: true,
            memetic: None,
        };
        assert_eq!(
            validate_creature_width(&creature),
            Err(CreatureWidthError::InvalidInputCount { found: 0 })
        );

        let creature = CreatureExport {
            input: 1,
            output: 0,
            neurons: vec![],
            synapses: vec![],
            semantic_version: None,
            forward_only: true,
            memetic: None,
        };
        assert_eq!(
            validate_creature_width(&creature),
            Err(CreatureWidthError::InvalidOutputCount { found: 0 })
        );
    }
}
