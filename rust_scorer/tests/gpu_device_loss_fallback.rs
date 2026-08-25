//! Issue #583 — a `wgpu` device lost mid-run degrades to CPU, it does not panic.
//!
//! On a headless fleet host the Vulkan/Mesa loader dropped the device during a
//! directory-mode run; `wgpu` panicked fatally inside `Device::poll`, the
//! process exited **101**, and the NEAT-AI batch bridge turned that into a
//! `ScorerStrictError` that killed a 1075-second evolve stage.
//!
//! The panic can only be reproduced with a real (and really failing) adapter,
//! so these tests **stub the device loss**: the GPU closure panics with the
//! exact payload from the stage-failure dump quoted in Issue #583, and the assertions are
//! on what the run boundary does with it — under `--gpu auto` a real CPU-scored
//! result (which `cli::main` serialises and exits 0 with), under `--gpu on` a
//! diagnostic error rather than an unwind.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use rust_scorer::cost::CostKind;
use rust_scorer::gpu::GpuBackendLabel;
use rust_scorer::gpu::GpuMode;
use rust_scorer::gpu::device_loss::{
    GpuRunFailure, catch_gpu_panic, message_reports_device_loss, run_with_device_loss_fallback,
};
use rust_scorer::multi_score::score_from_creature_dir_sampled;
use rust_scorer::sampling::SampleSpec;
use rust_scorer::scoring::ScoreResult;

/// The exact panic payload `wgpu` 29.0.4 raises when the parent device is lost
/// (quoted from the stage-failure dump in Issue #583).
const WGPU_DEVICE_LOST_PANIC: &str =
    "Error in Device::poll: Validation Error\n\nCaused by:\n  Parent device is lost\n";

fn minimal_creature(input: usize, output: usize) -> String {
    format!(
        r#"{{"input":{input},"output":{output},"forwardOnly":true,"neurons":[{{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}}],"synapses":[{{"fromUUID":"input-0","toUUID":"output-0","weight":0.5}}]}}"#
    )
}

fn write_training_data(dir: &Path, records: &[(Vec<f32>, Vec<f32>)]) {
    let mut file = std::fs::File::create(dir.join("0.bin")).expect("create data file");
    for (inputs, outputs) in records {
        for &v in inputs.iter().chain(outputs.iter()) {
            file.write_all(&v.to_le_bytes()).expect("write f32");
        }
    }
}

/// A real one-creature corpus on disk: `(tempdir, creatures_dir, data_dir)`.
fn corpus() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let creatures_dir = tmp.path().join("creatures");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&creatures_dir).expect("create creatures dir");
    std::fs::create_dir(&data_dir).expect("create data dir");
    std::fs::write(creatures_dir.join("alpha.json"), minimal_creature(1, 1))
        .expect("write creature");
    write_training_data(
        &data_dir,
        &[
            (vec![0.5], vec![0.25]),
            (vec![1.0], vec![0.5]),
            (vec![-0.5], vec![-0.25]),
        ],
    );
    (tmp, creatures_dir, data_dir)
}

fn cpu_scores(
    creatures_dir: &Path,
    data_dir: &Path,
) -> Result<BTreeMap<String, ScoreResult>, String> {
    score_from_creature_dir_sampled(
        creatures_dir,
        data_dir,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
        &SampleSpec::full(),
    )
}

/// Acceptance 2 + 4: under `--gpu auto` a device lost mid-run produces a real
/// CPU-scored result — identical to a plain CPU run — which serialises to the
/// same valid JSON the batch bridge parses. `cli::main` maps this `Ok` to a
/// `println!` of that JSON and exit 0.
#[test]
fn auto_device_loss_returns_a_cpu_scored_result() {
    let (_tmp, creatures_dir, data_dir) = corpus();

    let scored = run_with_device_loss_fallback(
        GpuMode::Auto,
        // Stubbed device loss: exactly what `wgpu` does mid-dispatch.
        || panic!("{WGPU_DEVICE_LOST_PANIC}"),
        || cpu_scores(&creatures_dir, &data_dir),
    )
    .expect("a lost device must degrade to the CPU pipeline, not abort the run");

    let expected = cpu_scores(&creatures_dir, &data_dir).expect("direct CPU run");
    assert_eq!(
        scored.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "the fallback must score every creature",
    );
    let alpha = scored.get("alpha").expect("missing creature alpha");
    let expected_alpha = expected.get("alpha").expect("missing creature alpha");
    assert_eq!(alpha.record_count, 3, "every record must still be scored");
    assert!(
        (alpha.error - expected_alpha.error).abs() < f64::EPSILON,
        "the fallback result must equal a plain CPU run: {} vs {}",
        alpha.error,
        expected_alpha.error,
    );
    assert_eq!(
        alpha.gpu_backend,
        GpuBackendLabel::CpuFallback,
        "gpuBackend must report what actually ran",
    );

    let json = serde_json::to_string(&scored).expect("the fallback result must serialise to JSON");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(
        parsed["alpha"]["gpuBackend"], "cpu-fallback",
        "batch callers must see cpu-fallback, got: {json}"
    );
}

/// Acceptance 3: the fallback is logged exactly once, on a single line, naming
/// the device loss and the CPU pipeline. The note is built by the same code the
/// run boundary prints, so an operator can grep the node log for it.
#[test]
fn device_loss_note_is_one_grepable_line() {
    let failure = catch_gpu_panic(|| panic!("{WGPU_DEVICE_LOST_PANIC}"))
        .expect_err("the device-lost panic must be caught");
    assert!(failure.is_device_lost(), "got {failure:?}");

    let reason = failure.to_string();
    assert!(
        !reason.contains('\n'),
        "the multi-line wgpu payload must flatten: {reason}"
    );
    assert!(
        reason.contains("Parent device is lost"),
        "the note must carry the underlying cause: {reason}"
    );
    assert!(
        reason.contains("environmental fault"),
        "the note must say this is environmental, not a bad creature: {reason}"
    );
}

/// Acceptance 2 (second half): `--gpu on` may still fail, but with a diagnostic
/// — a returned error the CLI exits non-zero on — rather than a panic. Reaching
/// the assertions at all proves the unwind was caught.
#[test]
fn on_reports_device_loss_as_a_diagnostic_not_a_panic() {
    let result: Result<BTreeMap<String, ScoreResult>, String> = run_with_device_loss_fallback(
        GpuMode::On,
        || panic!("{WGPU_DEVICE_LOST_PANIC}"),
        || panic!("--gpu on must not silently run the CPU pipeline"),
    );

    let err = result.expect_err("--gpu on must fail when the device is lost");
    assert!(
        err.contains("GPU device was lost mid-run"),
        "the diagnostic must name device loss, got: {err}"
    );
    assert!(
        err.contains("Parent device is lost"),
        "the diagnostic must carry the wgpu cause, got: {err}"
    );
}

/// The Issue #273 path is unchanged: a readback error the runner *returns*
/// still falls back under `auto` and still hard-errors under `on`.
#[test]
fn returned_readback_errors_keep_their_pre_583_behaviour() {
    let (_tmp, creatures_dir, data_dir) = corpus();
    let readback_failure = || Err("partials map_async failed: DeviceLost".to_string());

    let scored = run_with_device_loss_fallback(GpuMode::Auto, readback_failure, || {
        cpu_scores(&creatures_dir, &data_dir)
    })
    .expect("auto must fall back on a returned readback error");
    assert_eq!(scored.len(), 1);

    let err: Result<BTreeMap<String, ScoreResult>, String> =
        run_with_device_loss_fallback(GpuMode::On, readback_failure, || {
            cpu_scores(&creatures_dir, &data_dir)
        });
    assert_eq!(
        err.expect_err("on must surface the readback error"),
        "partials map_async failed: DeviceLost",
    );
}

/// A GPU abort that is *not* device loss must also leave `auto` with a result —
/// `auto` must never abort scoring — but is classified separately so the log
/// does not blame the environment for a bug.
#[test]
fn auto_survives_a_non_device_loss_gpu_abort() {
    let (_tmp, creatures_dir, data_dir) = corpus();

    let scored = run_with_device_loss_fallback(
        GpuMode::Auto,
        || panic!("n_records exceeds u32::MAX"),
        || cpu_scores(&creatures_dir, &data_dir),
    )
    .expect("auto must never abort scoring");
    assert_eq!(scored.len(), 1);

    assert_eq!(
        catch_gpu_panic(|| panic!("n_records exceeds u32::MAX")),
        Err(GpuRunFailure::Panicked(
            "n_records exceeds u32::MAX".to_string()
        )),
    );
    assert!(!message_reports_device_loss("n_records exceeds u32::MAX"));
}

/// End-to-end baseline for the destination of the fallback: the same corpus
/// scored through the real binary exits 0 with valid JSON and
/// `gpuBackend: cpu-fallback`. This is the state a lost device now lands in.
#[test]
fn cpu_directory_destination_exits_zero_with_valid_json() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let (_tmp, creatures_dir, data_dir) = corpus();

    let output = Command::new(bin)
        .arg("--gpu")
        .arg("auto")
        .arg(&creatures_dir)
        .arg(&data_dir)
        .output()
        .expect("spawn scorer");

    assert!(
        output.status.success(),
        "the CPU destination must exit 0, got {:?}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
    assert_eq!(parsed["alpha"]["recordCount"], 3);
}
