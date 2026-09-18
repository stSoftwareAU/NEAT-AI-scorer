//! neat-core 0.19.0 (core#685, Issue #682) — a synapse whose `toUUID` names no
//! listed neuron is refused instead of silently dropped.
//!
//! `compile_creature` groups synapses by `toUUID` and reads them back **per
//! listed neuron**, so a destination the creature does not carry was never
//! looked up: the edge vanished from the compiled network and `Ok` came back
//! with a network one synapse smaller than the creature declared. The scorer is
//! the engine that then reports a loss for a creature nobody wrote — exactly
//! the silent-divergence class `dual_role_parity.rs` guards on the source side.
//!
//! This suite pins the refusal from the scorer's own side, on both paths the
//! binary takes:
//!
//! 1. A dangling hidden destination is `CreatureError::UnknownTargetUuid`.
//! 2. A destination naming an **input** is refused too — `input-N` resolves as
//!    a source but is never a listed neuron.
//! 3. The same creature with that neuron listed still compiles, with **every**
//!    declared synapse present, so (1) cannot pass vacuously.
//! 4. The CLI fails loud — non-zero exit naming the offending UUID — rather
//!    than printing a score for the smaller network.
//!
//! Tests 1, 2 and 4 fail against a neat-core below 0.19.0, where each of these
//! creatures compiles `Ok` with the edge dropped.

use std::path::{Path, PathBuf};
use std::process::Command;

use neat_core::creature::{CreatureError, compile_creature, parse_creature_json};

/// The identity creature of `tests/fixtures/identity_creature.json`, plus the
/// extra synapse named by `to_uuid` and — when `list_hidden` — the hidden
/// neuron that synapse points at.
fn creature_json(to_uuid: &str, list_hidden: bool) -> String {
    let hidden = if list_hidden {
        r#",{"type":"hidden","uuid":"h-1","bias":0.0,"squash":"IDENTITY"}"#
    } else {
        ""
    };
    format!(
        r#"{{
  "input": 1,
  "output": 1,
  "forwardOnly": true,
  "semanticVersion": "4.0.0",
  "neurons": [
    {{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}}{hidden}
  ],
  "synapses": [
    {{"fromUUID":"input-0","toUUID":"output-0","weight":1.0}},
    {{"fromUUID":"input-0","toUUID":"{to_uuid}","weight":0.5}}
  ]
}}"#
    )
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn compile_error(to_uuid: &str) -> CreatureError {
    let creature = parse_creature_json(&creature_json(to_uuid, false)).expect("creature parses");
    compile_creature(&creature)
        .err()
        .unwrap_or_else(|| panic!("a synapse into unlisted `{to_uuid}` must be refused"))
}

#[test]
fn a_synapse_into_an_unlisted_neuron_is_refused() {
    assert!(
        matches!(compile_error("h-1"), CreatureError::UnknownTargetUuid(uuid) if uuid == "h-1"),
        "expected UnknownTargetUuid(\"h-1\")"
    );
}

#[test]
fn a_synapse_into_an_input_is_refused() {
    // `input-0` resolves as a *source*, so only the destination check catches
    // a transposed endpoint pair.
    assert!(
        matches!(compile_error("input-0"), CreatureError::UnknownTargetUuid(uuid) if uuid == "input-0"),
        "expected UnknownTargetUuid(\"input-0\")"
    );
}

#[test]
fn listing_the_destination_compiles_with_every_declared_synapse() {
    let creature = parse_creature_json(&creature_json("h-1", true)).expect("creature parses");
    let network = compile_creature(&creature).expect("a fully resolved creature compiles");
    assert_eq!(
        network.synapses().len(),
        creature.synapses.len(),
        "the compiled network must carry every synapse the creature declares"
    );
}

#[test]
fn the_cli_fails_loud_on_a_dangling_destination() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let creature_path = tmp.path().join("creature.json");
    std::fs::write(&creature_path, creature_json("h-1", false)).expect("write creature");

    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&data_dir).expect("create data dir");
    std::fs::copy(
        fixture("identity_data.bin"),
        data_dir.join("identity_data.bin"),
    )
    .expect("copy corpus fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .arg("--gpu")
        .arg("off")
        .arg(&creature_path)
        .arg(&data_dir)
        .output()
        .expect("failed to spawn rust_scorer binary");

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !output.status.success(),
        "the scorer must not score a creature it cannot compile as declared; stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains("h-1"),
        "stderr must name the unresolved destination, got:\n{stderr}"
    );
}
