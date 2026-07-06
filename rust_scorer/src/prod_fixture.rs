//! Production benchmark fixture loader — Issue #296.
//!
//! The Criterion suite in [`benches/scoring.rs`](../../benches/scoring.rs) must
//! gate every candidate optimisation (#297–#299) against the **production**
//! creature — the evolved GRQ-cluster network — not the synthetic 8→8→2 MLP
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
//! The pure functions here ([`parse_production_creature`],
//! [`check_production_topology`], [`load_production_creature`],
//! [`corpus_record_count`]) are unit-tested; the network fetch
//! ([`fetch_creature_to`]) shells out to `curl` and is exercised manually via
//! `./scripts/run-benches.sh`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use neat_core::creature::{CreatureExport, parse_creature_json};

/// Raw URL of the production GRQ-cluster creature.
///
/// Fetched at bench time rather than committed — the file is ≈ 3 MB and the
/// evolved creature is re-published (see the repo's `fetch.txt` date stamp), so
/// pinning a copy in-tree would drift silently.
pub const PRODUCTION_CREATURE_URL: &str =
    "https://raw.githubusercontent.com/stSoftwareAU/GRQ-cluster/main/network.json";

/// Environment variable pointing at a pre-downloaded production `network.json`.
///
/// When set, the bench loads from this path and skips the network fetch — the
/// documented way to reproduce offline / in an air-gapped environment.
pub const PROD_CREATURE_ENV: &str = "BENCH_PROD_CREATURE";

/// Default on-disk cache location (relative to the workspace root) for the
/// fetched creature, so repeated bench runs pay the download once.
pub const DEFAULT_CACHE_REL_PATH: &str = "target/bench-fixtures/grq-cluster-network.json";

// --- Expected production topology ranges -------------------------------------
//
// Ranges (not exact equality) so the assertion survives ordinary evolution of
// the GRQ-cluster creature while still rejecting a trivially small stand-in
// such as the synthetic 8→8→2 fixture (10 neurons, 8 inputs).

/// Minimum plausible input width for the production creature (observed 2461).
pub const MIN_INPUTS: usize = 1500;
/// Expected number of outputs for the production creature (observed 1).
pub const EXPECTED_OUTPUTS: usize = 1;
/// Lower bound on production neuron count (observed 1666).
pub const MIN_NEURONS: usize = 800;
/// Upper bound on production neuron count (guards against absurd inputs).
pub const MAX_NEURONS: usize = 12_000;
/// Lower bound on production synapse count (observed 21 510).
pub const MIN_SYNAPSES: usize = 8_000;
/// Upper bound on production synapse count.
pub const MAX_SYNAPSES: usize = 120_000;

// --- Production corpus sizing (from GRQ-cluster performance.csv) --------------

/// Total production training-corpus size in bytes (`training_data_size_bytes`).
pub const PRODUCTION_CORPUS_BYTES: u64 = 20_845_703_976;
/// Number of production training-data files (`training_data_files`).
pub const PRODUCTION_CORPUS_FILES: usize = 520;
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
pub fn parse_production_creature(json: &str) -> Result<CreatureExport, ProdFixtureError> {
    if json.trim().is_empty() {
        return Err(ProdFixtureError::Empty);
    }
    parse_creature_json(json).map_err(|e| ProdFixtureError::Parse(e.to_string()))
}

/// Assert the creature's topology sits within the expected production ranges.
///
/// Rejects a trivially small stand-in (e.g. the synthetic 8→8→2 fixture) so a
/// regression cannot quietly benchmark a meaningless creature.
pub fn check_production_topology(creature: &CreatureExport) -> Result<(), ProdFixtureError> {
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

/// Resolve the on-disk path the production creature should be read from.
///
/// Honours [`PROD_CREATURE_ENV`] when set; otherwise returns
/// `workspace_root/`[`DEFAULT_CACHE_REL_PATH`].
#[must_use]
pub fn resolve_creature_path(workspace_root: &Path) -> PathBuf {
    match std::env::var(PROD_CREATURE_ENV) {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => workspace_root.join(DEFAULT_CACHE_REL_PATH),
    }
}

/// Fetch the production creature to `dest` via `curl`, failing loud.
///
/// Uses `curl --fail` so an HTTP error (404/5xx) is a non-zero exit rather than
/// a written error page, and verifies the destination is non-empty afterwards.
/// Returns an error string (never falls back) on any failure.
pub fn fetch_creature_to(dest: &Path, url: &str) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create cache dir {}: {e}", parent.display()))?;
    }

    let status = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--output",
        ])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| format!("failed to run curl (is it installed?): {e}"))?;

    if !status.success() {
        return Err(format!(
            "curl exited with {status} fetching {url} — cannot benchmark the production creature"
        ));
    }

    let len = std::fs::metadata(dest)
        .map_err(|e| format!("fetched creature missing at {}: {e}", dest.display()))?
        .len();
    if len == 0 {
        return Err(format!(
            "fetched creature at {} is empty — refusing to benchmark",
            dest.display()
        ));
    }
    Ok(())
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

    #[test]
    fn resolve_creature_path_prefers_env_override() {
        // SAFETY: single-threaded test; restored below.
        unsafe { std::env::set_var(PROD_CREATURE_ENV, "/tmp/custom-prod.json") };
        let p = resolve_creature_path(Path::new("/workspace"));
        assert_eq!(p, PathBuf::from("/tmp/custom-prod.json"));
        unsafe { std::env::remove_var(PROD_CREATURE_ENV) };
        let p = resolve_creature_path(Path::new("/workspace"));
        assert_eq!(p, PathBuf::from("/workspace").join(DEFAULT_CACHE_REL_PATH));
    }
}
