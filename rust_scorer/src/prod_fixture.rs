//! Production benchmark fixture loader — Issue #296.
//!
//! The Criterion suite in [`benches/scoring.rs`](../../benches/scoring.rs) must
//! gate every candidate optimisation (#297–#299) against the **production**
//! creature — the evolved production network — not the synthetic 8→8→2 MLP
//! fixture. The production creature profiles very differently: ≈ 1666 neurons
//! across ≈ 34 distinct squash types and ≈ 21 510 synapses over 2461 inputs,
//! versus 10 neurons of pure `TANH`.
//!
//! ## Fail-loud contract
//!
//! A benchmark that silently falls back to the synthetic fixture would corrupt
//! every downstream A/B comparison, so this module never falls back. The load
//! path returns a hard [`ProdFixtureError`] (which the bench turns into a
//! panic) when the fixture is empty, fails to deserialize, or presents a
//! topology outside the expected production ranges — the latter catches a
//! regression that swaps in a trivially small creature.
//!
//! The pure functions here (`parse_production_creature`,
//! `check_production_topology`, [`load_production_creature`],
//! [`corpus_record_count`]) are unit-tested. The production creature itself is
//! **not** shipped and is **never fetched** by this public repo — it lives in a
//! private repository. A contributor supplies their own local copy via the
//! [`PROD_CREATURE_ENV`] environment variable; when it is unset the production
//! benches skip cleanly (see `./scripts/run-benches.sh`).

use std::fmt;
use std::path::PathBuf;

use neat_core::creature::{CreatureExport, parse_creature_json};

/// Environment variable pointing at a **local** production `network.json`.
///
/// The public repo ships no production creature and fetches nothing at bench
/// time. Set this to a local path to run the production benches; leave it unset
/// to skip them. This is the only way to point the benches at a production
/// creature — there is no remote default.
pub const PROD_CREATURE_ENV: &str = "BENCH_PROD_CREATURE";

// --- Expected production topology ranges -------------------------------------
//
// Ranges (not exact equality) so the assertion survives ordinary evolution of
// the production creature while still rejecting a trivially small stand-in
// such as the synthetic 8→8→2 fixture (10 neurons, 8 inputs).

/// Minimum plausible input width for the production creature (observed 2461).
pub(crate) const MIN_INPUTS: usize = 1500;
/// Expected number of outputs for the production creature (observed 1).
pub(crate) const EXPECTED_OUTPUTS: usize = 1;
/// Lower bound on production neuron count (observed 1666).
pub(crate) const MIN_NEURONS: usize = 800;
/// Upper bound on production neuron count (guards against absurd inputs).
pub(crate) const MAX_NEURONS: usize = 12_000;
/// Lower bound on production synapse count (observed 21 510).
pub(crate) const MIN_SYNAPSES: usize = 8_000;
/// Upper bound on production synapse count.
pub(crate) const MAX_SYNAPSES: usize = 120_000;

// --- Production corpus sizing (from the production performance.csv) ----------

/// Default corpus size the production bench builds when `BENCH_PROD_BYTES` is
/// unset — a runnable slice (64 MiB) of the ≈ 19.4 GiB production corpus. The
/// full production size is documented in `docs/performance-baseline.md`.
pub const DEFAULT_BENCH_CORPUS_BYTES: usize = 64 * 1024 * 1024;

/// A hard failure while loading the production benchmark fixture.
///
/// Every variant is fatal by design — the bench converts it into a panic so a
/// broken fixture can never masquerade as a valid production measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProdFixtureError {
    /// The fixture bytes were empty or whitespace-only.
    Empty,
    /// The fixture failed to deserialize as a creature.
    Parse(String),
    /// The creature deserialized but its topology is outside production ranges.
    Topology(String),
}

impl fmt::Display for ProdFixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(
                f,
                "production creature fixture is empty — refusing to fall back to the synthetic fixture"
            ),
            Self::Parse(msg) => write!(f, "production creature failed to deserialize: {msg}"),
            Self::Topology(msg) => write!(f, "production creature topology check failed: {msg}"),
        }
    }
}

impl std::error::Error for ProdFixtureError {}

/// Parse production creature JSON, failing loud on empty input.
///
/// Does **not** validate topology — see [`check_production_topology`]. Splitting
/// the two lets callers report the more specific failure.
pub(crate) fn parse_production_creature(json: &str) -> Result<CreatureExport, ProdFixtureError> {
    if json.trim().is_empty() {
        return Err(ProdFixtureError::Empty);
    }
    parse_creature_json(json).map_err(|e| ProdFixtureError::Parse(e.to_string()))
}

/// Assert the creature's topology sits within the expected production ranges.
///
/// Rejects a trivially small stand-in (e.g. the synthetic 8→8→2 fixture) so a
/// regression cannot quietly benchmark a meaningless creature.
pub(crate) fn check_production_topology(creature: &CreatureExport) -> Result<(), ProdFixtureError> {
    let neurons = creature.neurons.len();
    let synapses = creature.synapses.len();

    if !creature.forward_only {
        return Err(ProdFixtureError::Topology(
            "expected forwardOnly=true for the production creature".to_string(),
        ));
    }
    if creature.input < MIN_INPUTS {
        return Err(ProdFixtureError::Topology(format!(
            "input width {} is below the expected production minimum {MIN_INPUTS}",
            creature.input
        )));
    }
    if creature.output != EXPECTED_OUTPUTS {
        return Err(ProdFixtureError::Topology(format!(
            "output count {} does not match the expected production value {EXPECTED_OUTPUTS}",
            creature.output
        )));
    }
    if !(MIN_NEURONS..=MAX_NEURONS).contains(&neurons) {
        return Err(ProdFixtureError::Topology(format!(
            "neuron count {neurons} is outside the expected production range [{MIN_NEURONS}, {MAX_NEURONS}]"
        )));
    }
    if !(MIN_SYNAPSES..=MAX_SYNAPSES).contains(&synapses) {
        return Err(ProdFixtureError::Topology(format!(
            "synapse count {synapses} is outside the expected production range [{MIN_SYNAPSES}, {MAX_SYNAPSES}]"
        )));
    }
    Ok(())
}

/// Parse **and** topology-check the production creature in one fail-loud step.
pub fn load_production_creature(json: &str) -> Result<CreatureExport, ProdFixtureError> {
    let creature = parse_production_creature(json)?;
    check_production_topology(&creature)?;
    Ok(creature)
}

/// Number of whole records a corpus of `total_bytes` holds for a creature with
/// `num_inputs` inputs and `num_outputs` outputs (packed little-endian `f32`).
///
/// At least one record is always returned so a tiny `BENCH_PROD_BYTES` still
/// produces a non-empty corpus.
#[must_use]
pub fn corpus_record_count(total_bytes: usize, num_inputs: usize, num_outputs: usize) -> usize {
    let record_bytes = (num_inputs + num_outputs) * std::mem::size_of::<f32>();
    debug_assert!(record_bytes > 0, "record must hold at least one value");
    (total_bytes / record_bytes).max(1)
}

/// Resolve the **local** production creature path from [`PROD_CREATURE_ENV`].
///
/// Returns `Some(path)` only when the variable is set to a non-empty value;
/// otherwise `None`, signalling the caller to skip the production benches. There
/// is deliberately **no** default (remote or on-disk): the public repo is
/// self-contained and never reaches for a private-repo creature at runtime.
#[must_use]
pub fn production_creature_path_from_env() -> Option<PathBuf> {
    match std::env::var(PROD_CREATURE_ENV) {
        Ok(p) if !p.trim().is_empty() => Some(PathBuf::from(p)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal creature JSON with the given counts, all forward-only.
    fn creature_json(input: usize, output: usize, hidden: usize, synapses: usize) -> String {
        let mut neurons: Vec<String> = Vec::new();
        for h in 0..hidden {
            neurons.push(format!(
                r#"{{"type":"hidden","uuid":"hidden-{h}","bias":0.0,"squash":"TANH"}}"#
            ));
        }
        for o in 0..output {
            neurons.push(format!(
                r#"{{"type":"output","uuid":"output-{o}","bias":0.0,"squash":"IDENTITY"}}"#
            ));
        }
        let mut syn: Vec<String> = Vec::new();
        for s in 0..synapses {
            let h = if hidden > 0 { s % hidden } else { 0 };
            syn.push(format!(
                r#"{{"fromUUID":"input-{}","toUUID":"hidden-{h}","weight":0.1}}"#,
                s % input.max(1)
            ));
        }
        format!(
            r#"{{"input":{input},"output":{output},"forwardOnly":true,"semanticVersion":"4.0.0","neurons":[{}],"synapses":[{}]}}"#,
            neurons.join(","),
            syn.join(",")
        )
    }

    #[test]
    fn parse_rejects_empty() {
        assert_eq!(
            parse_production_creature("   \n"),
            Err(ProdFixtureError::Empty)
        );
        assert_eq!(parse_production_creature(""), Err(ProdFixtureError::Empty));
    }

    #[test]
    fn parse_rejects_garbage() {
        match parse_production_creature("{not json") {
            Err(ProdFixtureError::Parse(_)) => {}
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn topology_accepts_production_sized_creature() {
        // 2461 inputs, 1 output, 1666 neurons, 21_510 synapses (observed prod).
        let json = creature_json(2461, 1, 1665, 21_510);
        let creature = parse_production_creature(&json).expect("parse");
        assert!(check_production_topology(&creature).is_ok());
        // Full load path also succeeds.
        assert!(load_production_creature(&json).is_ok());
    }

    #[test]
    fn topology_rejects_synthetic_fixture() {
        // The synthetic 8→8→2 fixture must never pass as production.
        let json = creature_json(8, 2, 8, 16);
        let creature = parse_production_creature(&json).expect("parse");
        match check_production_topology(&creature) {
            Err(ProdFixtureError::Topology(_)) => {}
            other => panic!("expected Topology error for synthetic fixture, got {other:?}"),
        }
        // And the combined load path rejects it too.
        assert!(matches!(
            load_production_creature(&json),
            Err(ProdFixtureError::Topology(_))
        ));
    }

    #[test]
    fn topology_rejects_non_forward_only() {
        let json = creature_json(2461, 1, 1665, 21_510)
            .replace("\"forwardOnly\":true", "\"forwardOnly\":false");
        let creature = parse_production_creature(&json).expect("parse");
        match check_production_topology(&creature) {
            Err(ProdFixtureError::Topology(msg)) => assert!(msg.contains("forwardOnly")),
            other => panic!("expected forwardOnly Topology error, got {other:?}"),
        }
    }

    #[test]
    fn topology_rejects_wrong_output_count() {
        let json = creature_json(2461, 2, 1665, 21_510);
        let creature = parse_production_creature(&json).expect("parse");
        match check_production_topology(&creature) {
            Err(ProdFixtureError::Topology(msg)) => assert!(msg.contains("output")),
            other => panic!("expected output-count Topology error, got {other:?}"),
        }
    }

    #[test]
    fn corpus_record_count_matches_hand_calculation() {
        // 2462 f32 per record → 9848 bytes/record.
        let record_bytes = (2461 + 1) * 4;
        let total = 64 * 1024 * 1024;
        assert_eq!(corpus_record_count(total, 2461, 1), total / record_bytes);
    }

    #[test]
    fn corpus_record_count_is_never_zero() {
        assert_eq!(corpus_record_count(1, 2461, 1), 1);
    }

    // Business-logic change (Issue #448): the private-repo fetch and its on-disk
    // cache default were deleted, so `resolve_creature_path` (which returned a
    // default cache path) no longer exists. Its replacement,
    // `production_creature_path_from_env`, resolves *only* a local override and
    // returns `None` when unset — there is no remote or on-disk default. This
    // test replaces the former `resolve_creature_path_prefers_env_override`.
    #[test]
    fn production_creature_path_reads_local_env_only() {
        // SAFETY: single-threaded test; each case restores the variable.
        unsafe { std::env::remove_var(PROD_CREATURE_ENV) };
        assert_eq!(
            production_creature_path_from_env(),
            None,
            "unset must yield None (production benches skip; no private-repo default)"
        );

        unsafe { std::env::set_var(PROD_CREATURE_ENV, "   ") };
        assert_eq!(
            production_creature_path_from_env(),
            None,
            "blank must yield None"
        );

        unsafe { std::env::set_var(PROD_CREATURE_ENV, "/tmp/custom-prod.json") };
        assert_eq!(
            production_creature_path_from_env(),
            Some(PathBuf::from("/tmp/custom-prod.json"))
        );

        unsafe { std::env::remove_var(PROD_CREATURE_ENV) };
    }
}
