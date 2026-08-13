//! Issue #475: the `rust_scorer` binary and the `rust_scorer` library must be
//! one implementation, not two copies.
//!
//! `src/main.rs` used to declare the whole `mod` tree itself, so the bin target
//! compiled a **second, independent** copy of every module: a change to the
//! library side was not necessarily what the shipped binary ran. `main.rs` is
//! now a thin shim over `rust_scorer::cli::main`.
//!
//! These tests assert the invariant behaviourally — the JSON the *binary*
//! prints must match, value for value, what the *library* entry points return
//! for the same inputs. Any future re-duplication that let the two drift shows
//! up here as a numeric mismatch. Scored floats are compared within
//! [`MAX_BIN_LIB_ULP_DRIFT`], the cost of code-generating one implementation
//! into two artefacts; the structural fields are compared exactly.

use std::io::Write;
use std::path::Path;
use std::process::Command;

use rust_scorer::cost::CostKind;
use rust_scorer::fixture_json::{creature_envelope, neuron_json, synapse_json};
use rust_scorer::gpu::GpuBackendLabel;
use rust_scorer::multi_score::score_from_creature_dir;

/// Largest relative gap between the binary's and the library's number that is
/// still "one implementation" rather than two.
///
/// The upstream loss kernels reduce their SIMD lanes with `Simd::reduce_sum`,
/// whose summation order `std::simd` deliberately leaves **unspecified** for
/// floats. The binary and the test harness are separately code-generated
/// artefacts, so LLVM may lower that reduction differently in each and the last
/// bit of a scored `error` can differ — measured at 1 ULP on this fixture once
/// `NEAT-AI-Lamarck#130` routed a short chunk through the 4-record path
/// (before that a 4-record fixture ran the scalar path, where no reduction
/// order exists to differ).
///
/// A few ULP is what separate code generation of the *same* implementation can
/// cost. Re-duplicating the module tree — the regression these tests exist to
/// catch — moves a score by orders of magnitude more, and the exact structural
/// fields (`recordCount`, `complexityPenalty`, `costName`) are still asserted
/// bit-for-bit below.
const MAX_BIN_LIB_ULP_DRIFT: f64 = 4.0 * f64::EPSILON;

/// Assert the binary and the library agree on a scored number.
fn assert_agrees(from_binary: f64, from_library: f64, what: &str) {
    if from_binary == from_library {
        return;
    }
    let rel = (from_binary - from_library).abs() / from_library.abs().max(f64::MIN_POSITIVE);
    assert!(
        rel <= MAX_BIN_LIB_ULP_DRIFT,
        "{what}: binary {from_binary} and library {from_library} differ by {rel:e} relative — \
         beyond the {MAX_BIN_LIB_ULP_DRIFT:e} separate-code-generation allowance, so they are \
         running different implementations"
    );
}

/// Write `records` as one packed little-endian `f32` `.bin` file.
fn write_training_data(dir: &Path, records: &[(Vec<f32>, Vec<f32>)]) {
    let mut file = std::fs::File::create(dir.join("0.bin")).expect("create data file");
    for (inputs, outputs) in records {
        for &v in inputs.iter().chain(outputs.iter()) {
            file.write_all(&v.to_le_bytes()).expect("write f32");
        }
    }
}

/// Forward-only creature with one hidden TANH neuron, so the scored value is
/// sensitive to the whole activation pipeline (not just an identity pass).
fn creature_json(weight: f64) -> String {
    creature_envelope(
        1,
        1,
        &[
            neuron_json("hidden", "hidden-0", 0.25, "TANH"),
            neuron_json("output", "output-0", 0.0, "IDENTITY"),
        ],
        &[
            synapse_json("input-0", "hidden-0", weight),
            synapse_json("hidden-0", "output-0", 0.75),
        ],
    )
}

/// Build a temp directory holding `<root>/creatures/{a,b}.json` and
/// `<root>/data/0.bin`, and return the temp dir plus both paths.
fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let creatures = tmp.path().join("creatures");
    let data = tmp.path().join("data");
    std::fs::create_dir(&creatures).expect("mkdir creatures");
    std::fs::create_dir(&data).expect("mkdir data");
    std::fs::write(creatures.join("a.json"), creature_json(1.3)).expect("write creature a");
    std::fs::write(creatures.join("b.json"), creature_json(-0.4)).expect("write creature b");
    write_training_data(
        &data,
        &[
            (vec![0.25], vec![0.0]),
            (vec![0.75], vec![1.0]),
            (vec![-0.5], vec![0.5]),
            (vec![1.0], vec![0.9]),
        ],
    );
    (tmp, creatures, data)
}

/// Run the binary in directory mode with `--gpu off` and return its parsed JSON.
fn run_binary(creatures: &Path, data: &Path, cost: &str) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .arg("--gpu")
        .arg("off")
        .arg("--cost")
        .arg(cost)
        .arg(creatures)
        .arg(data)
        .output()
        .expect("run rust_scorer");
    assert!(
        out.status.success(),
        "binary failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("binary must print valid JSON")
}

/// The binary's directory-mode result must match the library's
/// `score_from_creature_dir` — same scoring code, so the same numbers to
/// within [`MAX_BIN_LIB_ULP_DRIFT`], and the same structural fields exactly.
#[test]
fn binary_directory_scores_match_library_entry_point() {
    let (_tmp, creatures, data) = fixture();

    let from_binary = run_binary(&creatures, &data, "MSE");
    let from_library = score_from_creature_dir(
        &creatures,
        &data,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
    )
    .expect("library scoring must succeed");

    assert_eq!(
        from_library.len(),
        2,
        "fixture must score both creatures, got {from_library:?}"
    );
    for (id, lib) in &from_library {
        let bin = from_binary
            .get(id)
            .unwrap_or_else(|| panic!("binary output is missing creature '{id}': {from_binary}"));
        assert_agrees(
            bin["error"].as_f64().expect("error must be a number"),
            lib.error,
            &format!("error for '{id}'"),
        );
        assert_agrees(
            bin["score"].as_f64().expect("score must be a number"),
            lib.score,
            &format!("score for '{id}'"),
        );
        assert_eq!(
            bin["recordCount"]
                .as_u64()
                .expect("recordCount must be a number"),
            lib.record_count as u64,
            "recordCount drift for '{id}' between binary and library"
        );
        assert_eq!(
            bin["complexityPenalty"]
                .as_f64()
                .expect("complexityPenalty must be a number"),
            lib.complexity_penalty,
            "complexityPenalty drift for '{id}' between binary and library"
        );
    }
}

/// The same invariant under a non-default `--cost`, so the shared cost dispatch
/// (not just the MSE default) is proven to come from one implementation.
#[test]
fn binary_and_library_agree_for_non_default_cost() {
    let (_tmp, creatures, data) = fixture();

    let from_binary = run_binary(&creatures, &data, "MAE");
    let from_library = score_from_creature_dir(
        &creatures,
        &data,
        GpuBackendLabel::CpuFallback,
        CostKind::Mae,
    )
    .expect("library scoring must succeed");

    for (id, lib) in &from_library {
        let bin = from_binary
            .get(id)
            .unwrap_or_else(|| panic!("binary output is missing creature '{id}': {from_binary}"));
        assert_agrees(
            bin["error"].as_f64().expect("error must be a number"),
            lib.error,
            &format!("MAE error for '{id}'"),
        );
        assert_eq!(
            bin["costName"].as_str(),
            Some("MAE"),
            "binary must echo the resolved cost name"
        );
    }
}
