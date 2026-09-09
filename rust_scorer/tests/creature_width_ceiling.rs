//! neat-core 0.14.0 (NEAT-AI-core#640, Issue #622) — a declared observation
//! width above `MAX_NODE_COUNT` is refused *before* anything is sized by it.
//!
//! `CreatureExport::input` is a declared count with no backing data, so a
//! sub-100-byte creature saying `"input": 100000000` used to make
//! `parse_creature_json` / `compile_creature` build one owned String UUID per
//! declared input. neat-core now bounds the width in `validate_creature_width`,
//! which is the BREAKING half of the 0.13.0 -> 0.14.0 bump this repository
//! acknowledges in `neat-core.expected-version`.
//!
//! rust_scorer needs no code change: every load path calls
//! `parse_creature_json` first, so the ceiling arrives ahead of the scorer's
//! own lower-bound guard (`rust_scorer::creature_width`, Issue #571). These
//! tests pin that from the binary's side, so a neat-core that ever dropped the
//! ceiling fails here rather than in the fleet.

use std::path::{Path, PathBuf};
use std::process::Command;

use neat_core::creature::{CreatureError, parse_creature_json};
use neat_core::network::MAX_NODE_COUNT;
use rust_scorer::creature_width::validate_creature_width;
use rust_scorer::fixture_json::{creature_envelope, neuron_json, synapse_json};

/// A creature whose top-level `input` is set explicitly; `neurons` lists only
/// the single output, so the declared width is the sole allocation driver.
fn creature_json(input: usize) -> String {
    let neurons = vec![neuron_json("output", "output-0", 0.0, "IDENTITY")];
    let synapses = vec![synapse_json("input-0", "output-0", 1.0)];
    creature_envelope(input, 1, &neurons, &synapses)
}

fn scorer_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rust_scorer"))
}

fn missing_data_dir(tmp: &Path) -> PathBuf {
    let missing = tmp.join("no-such-data-dir");
    assert!(!missing.exists());
    missing
}

#[test]
fn a_declared_width_past_the_ceiling_is_refused_by_the_parser() {
    let err = parse_creature_json(&creature_json(100_000_000)).unwrap_err();
    // The typed error carries the whole declared node count (input + listed
    // neurons), not just the input.
    assert!(
        matches!(err, CreatureError::TooManyNodes { count } if count == 100_000_001),
        "expected TooManyNodes carrying the declared node count, got: {err:?}"
    );
    assert!(
        err.to_string().contains("exceeding the maximum of"),
        "the rendered message must name the ceiling, got: {err}"
    );
}

#[test]
fn the_ceiling_is_inclusive_at_max_node_count() {
    // MAX_NODE_COUNT is the widest network a u16 source index can address, so
    // it is accepted; one more is not.
    assert!(parse_creature_json(&creature_json(MAX_NODE_COUNT)).is_ok());
    assert!(matches!(
        parse_creature_json(&creature_json(MAX_NODE_COUNT + 1)),
        Err(CreatureError::TooManyNodes { .. })
    ));
}

#[test]
fn the_scorer_lower_bound_guard_still_accepts_a_legal_width() {
    // The scorer's own guard (Issue #571) is unchanged by the bump: it owns the
    // `< 1` wording, and the ceiling is neat-core's.
    let creature = parse_creature_json(&creature_json(3)).unwrap();
    assert!(validate_creature_width(&creature).is_ok());
}

#[test]
fn the_cli_refuses_an_oversized_width_before_reading_data() {
    let tmp = tempfile::tempdir().unwrap();
    let creature = tmp.path().join("creature.json");
    std::fs::write(&creature, creature_json(100_000_000)).unwrap();

    let output = Command::new(scorer_bin())
        .arg(&creature)
        .arg(missing_data_dir(tmp.path()))
        .output()
        .expect("spawn scorer");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "scorer must exit non-zero, stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains("exceeding the maximum of"),
        "stderr must carry the ceiling error, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("is not a directory") && !stderr.contains("No .bin files"),
        "the ceiling must fire before the data directory is touched, got:\n{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "no score JSON may be emitted for an oversized creature, got:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
