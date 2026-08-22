//! stSoftwareAU/NEAT-AI#3813 — a real evolved creature export carrying a
//! populated `memetic` block parses through the **binary's** own path.
//!
//! Every NEAT-AI evolve run exports `memetic.weights` as a JSON array — usually
//! the empty array `[]`, because a creature evolved without a memetic pass still
//! writes the key. Against a map-only `MemeticExport::weights` the CLI died
//! before doing any work:
//!
//! ```text
//! Error: Creature JSON error: invalid type: sequence, expected a map at line 1 column 567
//! ```
//!
//! NEAT-AI evolve then fell back to WASM scoring hundreds of times per
//! `./quality.sh`, so the native batch path — the whole point of this binary —
//! was effectively dead while every run still looked green.
//!
//! `rust_scorer/tests/memetic_wire_forms_scoring.rs` covers the two wire forms
//! at the *library* boundary with JSON built in-process. This file is the
//! regression gate for the shapes that actually fire in the wild, fed to the
//! **compiled binary** through committed fixtures — the same
//! `fs::read_to_string` → `parse_creature_json` path
//! `rust_scorer <creature.json> <data_dir>` takes:
//!
//! * `memetic.weights: []` — the empty row array, the shape on essentially
//!   every evolved creature.
//! * `memetic.weights: [{fromUUID, toUUID, weight}, …]` — the populated row
//!   array.
//! * `memetic.ancestry[].weights` — the same two shapes one snapshot deeper.
//!
//! These assertions cannot hold against a `neat-core` older than 0.10.0: the
//! row form is rejected there with `invalid type: sequence, expected a map`.

use std::path::{Path, PathBuf};
use std::process::Command;

use neat_core::creature::parse_creature_json;

/// The serde message the map-only deserialiser produced for every memetic
/// creature. Named once so each assertion below points at the same regression.
const SEQUENCE_EXPECTED_MAP: &str = "invalid type: sequence, expected a map";

/// Both fixtures are the identity creature the smoke test already scores, plus
/// a `memetic` block — so a parse regression is the only way they can fail.
const MEMETIC_FIXTURES: [&str; 2] = [
    "memetic_creature.json",
    "memetic_creature_empty_weights.json",
];

/// Resolve `tests/fixtures/<name>` relative to this crate.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// A fresh temporary directory holding a copy of `identity_data.bin`, so
/// concurrent tests never share a data directory.
fn identity_data_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let src = fixture("identity_data.bin");
    let dst = tmp.path().join("identity_data.bin");
    std::fs::copy(&src, &dst)
        .unwrap_or_else(|e| panic!("copy fixture {} -> {}: {e}", src.display(), dst.display()));
    tmp
}

/// Run the compiled `rust_scorer` against a fixture creature and a data
/// directory, with the GPU explicitly off (the manual reproduction in
/// NEAT-AI#3810 uses `--gpu off`, and it keeps the test hardware-independent).
fn run_scorer(creature: &str, data_dir: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .arg("--gpu")
        .arg("off")
        .arg(fixture(creature))
        .arg(data_dir)
        .output()
        .expect("failed to spawn rust_scorer binary")
}

#[test]
fn cli_scores_a_creature_carrying_a_populated_memetic_block() {
    for name in MEMETIC_FIXTURES {
        let data_dir = identity_data_dir();
        let output = run_scorer(name, data_dir.path());
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        assert!(
            !stderr.contains(SEQUENCE_EXPECTED_MAP),
            "{name}: the memetic parse regression is back:\n{stderr}"
        );
        assert!(
            output.status.success(),
            "{name}: rust_scorer exited with {:?}\nstderr:\n{stderr}",
            output.status.code(),
        );

        let stdout = String::from_utf8(output.stdout).expect("stdout is utf-8");
        let parsed: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("{name}: stdout is not JSON: {e}\n{stdout}"));

        // The memetic block is metadata the scorer never applies, so the
        // identity creature must still score exactly as it does without one.
        let error = parsed
            .get("error")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or_else(|| panic!("{name}: missing `error` in scorer JSON"));
        assert!(
            error.abs() < 1e-6,
            "{name}: expected near-zero error, got {error}"
        );

        let record_count = parsed
            .get("recordCount")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| panic!("{name}: missing `recordCount` in scorer JSON"));
        assert_eq!(record_count, 4, "{name}: expected 4 records");
    }
}

/// The manual reproduction from NEAT-AI#3810, as a test: pointed at a data
/// directory with no `.bin` files, the binary must get *past* the creature
/// parse and fail on the missing corpus instead.
#[test]
fn cli_reaches_the_corpus_check_instead_of_dying_on_the_memetic_block() {
    for name in MEMETIC_FIXTURES {
        let empty_dir = tempfile::tempdir().expect("create tempdir");
        let output = run_scorer(name, empty_dir.path());
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        assert!(
            !stderr.contains(SEQUENCE_EXPECTED_MAP),
            "{name}: the memetic parse regression is back:\n{stderr}"
        );
        assert!(
            stderr.contains("No .bin files found in training data directory"),
            "{name}: expected the corpus check to be the first failure, got:\n{stderr}"
        );
    }
}

#[test]
fn an_empty_memetic_weight_array_parses_to_no_rows() {
    let json = std::fs::read_to_string(fixture("memetic_creature_empty_weights.json"))
        .expect("read the empty-weights fixture");
    let creature = parse_creature_json(&json).expect("`\"weights\": []` must parse");

    let memetic = creature
        .memetic
        .as_ref()
        .expect("memetic survives the parse");
    let rows = memetic
        .weights
        .rows()
        .expect("an empty array is the row form, not the map form");
    assert!(
        rows.is_empty(),
        "an empty array carries no rows, got {rows:?}"
    );
    assert_eq!(
        memetic.biases.get("output-0").copied(),
        Some(-0.1116),
        "the rest of the memetic block survives alongside the empty array"
    );
}

#[test]
fn a_populated_memetic_weight_array_keeps_its_rows() {
    let json = std::fs::read_to_string(fixture("memetic_creature.json"))
        .expect("read the populated-weights fixture");
    let creature = parse_creature_json(&json).expect("the populated row array must parse");

    let memetic = creature
        .memetic
        .as_ref()
        .expect("memetic survives the parse");
    let rows = memetic
        .weights
        .rows()
        .expect("the row array is the row form");
    assert_eq!(rows.len(), 1, "the fixture carries one weight row");
    assert_eq!(rows[0].from_uuid.as_deref(), Some("input-0"));
    assert_eq!(rows[0].to_uuid.as_deref(), Some("output-0"));
}

/// `memetic.ancestry[]` is another memetic record and carries `weights` in the
/// same array form — including the empty array. Both fixtures must keep every
/// snapshot, with its rows intact.
#[test]
fn ancestry_snapshots_keep_their_array_form_weights() {
    for name in MEMETIC_FIXTURES {
        let json = std::fs::read_to_string(fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let creature = parse_creature_json(&json)
            .unwrap_or_else(|e| panic!("{name}: array-form ancestry weights must parse: {e}"));

        let ancestry = creature
            .memetic
            .as_ref()
            .expect("memetic survives the parse")
            .extra
            .get("ancestry")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("{name}: the ancestry list survives the parse"));
        assert_eq!(ancestry.len(), 2, "{name}: both snapshots survive");

        let populated = ancestry[0]
            .get("weights")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("{name}: the first snapshot keeps array-form weights"));
        assert_eq!(populated.len(), 1, "{name}: its single row survives");

        let empty = ancestry[1]
            .get("weights")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("{name}: the second snapshot keeps array-form weights"));
        assert!(empty.is_empty(), "{name}: its empty array survives");
    }
}
