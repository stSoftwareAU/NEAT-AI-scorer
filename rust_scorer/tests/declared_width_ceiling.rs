//! Issue #609 — the declared observation width is bounded before it is walked.
//!
//! `neat-core` 0.14.0 (NEAT-AI-core#622 / PR #640) added the `MAX_NODE_COUNT`
//! ceiling to `validate_creature_width`, the single home of the width rule. A
//! declared `input` above that ceiling is now `CreatureError::TooManyNodes`,
//! refused **before** any allocation is sized by it — `parse_creature_json`,
//! `compile_creature`, the two serialisers, `validate_creature_topology` and
//! `cleanup_creature_with` all inherit the refusal. Pre-1.0 that is a minor
//! bump, so the scorer had to acknowledge it in `neat-core.expected-version`.
//!
//! The bump is a **behavioural narrowing, not a signature change**, and this
//! suite is the evidence that the narrowing cannot reject a creature the fleet
//! actually scores:
//!
//! 1. **The refusal is typed and reaches the scorer's own boundary.** An absurd
//!    declared width fails on parse and on compile with `TooManyNodes`, and the
//!    CLI reports it and scores nothing. Only the *parse-path* refusal is new:
//!    `compile_creature` has refused the same width since Issue #177, through
//!    the total-node check described in point 3 — what 0.14.0 adds there is
//!    that the refusal now happens **before** the walk.
//! 2. **The ceiling is inclusive.** `input == MAX_NODE_COUNT` still parses, so
//!    the narrowing starts exactly one input past the widest addressable
//!    network.
//! 3. **Nothing compilable was lost.** The widest creature that has ever
//!    compiled — total nodes exactly `MAX_NODE_COUNT`, the Issue #177 ceiling
//!    `compile_creature` has enforced since long before this bump — still
//!    compiles. Every creature the new width check refuses would already have
//!    earned the same `TooManyNodes` from that total-node check, because the
//!    total (`input + neurons.len()`) is never below `input`; 0.14.0 only moves
//!    the refusal earlier, ahead of the per-input allocation.
//! 4. **Production width is nowhere near it.** The production creature's 2461
//!    inputs (`rust_scorer::prod_fixture`) parse, compile and keep their exact
//!    width, twenty-six times below the ceiling.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use neat_core::creature::{CreatureError, compile_creature, parse_creature_json};
use neat_core::network::MAX_NODE_COUNT;
use rust_scorer::fixture_json::{creature_envelope, dense_mlp_creature_json, neuron_json};

/// The production observation width, as recorded in `rust_scorer::prod_fixture`.
const PRODUCTION_INPUTS: usize = 2461;

/// A minimal forward-only creature declaring `inputs` observations and exactly
/// one output neuron, with no synapses. `neurons` lists only non-input
/// neurons, so the declared width is carried by the top-level `input` alone —
/// which is precisely why it needs its own ceiling.
fn creature_json(inputs: usize) -> String {
    let neurons = vec![neuron_json("output", "output-0", 0.0, "IDENTITY")];
    creature_envelope(inputs, 1, &neurons, &[])
}

#[test]
fn absurd_declared_width_is_refused_on_parse_with_the_typed_error() {
    // Under 200 bytes of JSON declaring a hundred million inputs: before
    // neat-core 0.14.0 this was walked into a hundred million map entries.
    let json = creature_json(100_000_000);
    assert!(
        json.len() < 200,
        "the fixture must stay a tiny payload declaring an enormous width"
    );

    let err = parse_creature_json(&json).expect_err("a width past the ceiling must be refused");
    // `count` is `input` plus the listed (non-input) neurons — the same
    // declared node count the post-compile ceiling reports.
    assert!(
        matches!(err, CreatureError::TooManyNodes { count } if count == 100_000_001),
        "the width path must report the typed node-count error, got: {err}"
    );
}

#[test]
fn no_boundary_accepts_a_declared_width_past_the_ceiling() {
    // The parse boundary refuses it (new in 0.14.0) ...
    let json = creature_json(MAX_NODE_COUNT + 1);
    let err = parse_creature_json(&json).expect_err("parse must refuse it");
    assert!(matches!(err, CreatureError::TooManyNodes { .. }));

    // ... and so does the compile boundary, which cannot be reached through
    // the parser any more. Widen an export the parser did produce, so compile
    // is exercised on the same shape. This half is the Issue #177 total-node
    // guarantee, unchanged by the bump: it is here so the suite pins that
    // *no* boundary accepts the width, not just the one that moved.

    let mut creature = parse_creature_json(&creature_json(MAX_NODE_COUNT))
        .expect("the ceiling itself parses, giving a valid export to widen");
    creature.input = MAX_NODE_COUNT + 1;
    // `CompiledNetwork` is not `Debug`, so match the result rather than
    // `expect_err`-ing it.
    let err = match compile_creature(&creature) {
        Ok(_) => panic!("compile must refuse the widened export"),
        Err(err) => err,
    };
    assert!(
        matches!(err, CreatureError::TooManyNodes { count } if count == MAX_NODE_COUNT + 2),
        "compile must refuse it before sizing anything by the declared width, got: {err}"
    );
}

#[test]
fn the_ceiling_is_inclusive() {
    // MAX_NODE_COUNT inputs is the widest addressable observation vector, so it
    // is accepted; one more is not. Parsing allocates nothing per input, so the
    // acceptance here is cheap.
    let at_ceiling = parse_creature_json(&creature_json(MAX_NODE_COUNT))
        .expect("a width exactly at the ceiling must still parse");
    assert_eq!(at_ceiling.input, MAX_NODE_COUNT);

    assert!(
        parse_creature_json(&creature_json(MAX_NODE_COUNT + 1)).is_err(),
        "one input past the ceiling must be refused"
    );
}

#[test]
fn the_widest_compilable_creature_still_compiles() {
    // Total nodes = MAX_NODE_COUNT - 1 inputs + 1 output = MAX_NODE_COUNT, the
    // Issue #177 ceiling `compile_creature` has always allowed. If the 0.14.0
    // width check could reject a creature that used to compile, it would reject
    // this one.
    let creature = parse_creature_json(&creature_json(MAX_NODE_COUNT - 1))
        .expect("the widest compilable width must parse");
    let network = compile_creature(&creature).expect("the widest compilable width must compile");
    assert_eq!(network.num_inputs(), MAX_NODE_COUNT - 1);
    assert_eq!(network.num_neurons(), MAX_NODE_COUNT);
}

#[test]
fn production_width_parses_and_compiles_unchanged() {
    let json = dense_mlp_creature_json(PRODUCTION_INPUTS, 1, 2, "TANH");
    let creature = parse_creature_json(&json).expect("the production width must parse");
    assert_eq!(creature.input, PRODUCTION_INPUTS);

    let network = compile_creature(&creature).expect("the production width must compile");
    assert_eq!(network.num_inputs(), PRODUCTION_INPUTS);
    // Compile-time: the production width can never reach the ceiling.
    const { assert!(PRODUCTION_INPUTS < MAX_NODE_COUNT) };
}

#[test]
fn the_cli_reports_the_refusal_and_scores_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let creature_path = tmp.path().join("absurd-width.json");
    let mut file = std::fs::File::create(&creature_path).unwrap();
    file.write_all(creature_json(100_000_000).as_bytes())
        .unwrap();
    drop(file);

    // A data directory that does not exist: the width refusal must fire first,
    // so no training byte is ever opened.
    let missing = tmp.path().join("no-such-data-dir");
    assert!(!missing.exists());

    let output = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_rust_scorer")))
        .arg(&creature_path)
        .arg(&missing)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("scorer binary must run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "an unhostable declared width must exit non-zero, stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains("100000001 nodes")
            && stderr.contains(&format!("maximum of {MAX_NODE_COUNT}")),
        "stderr must carry the typed node-count refusal, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("is not a directory") && !stderr.contains("No .bin files"),
        "the refusal must fire before the data directory is touched, got:\n{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "no score JSON may be emitted, got:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
