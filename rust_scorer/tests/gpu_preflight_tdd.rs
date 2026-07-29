//! Issue #180 — CPU-only GPU compatibility pre-flight for directory mode.
//!
//! `gpu_directory_compatible` decides — without creating a `wgpu` device —
//! whether a GPU kernel can host every creature in a directory. `cli.rs`
//! consults it under `--gpu auto`.
//!
//! Issue #182 lifted the 256-neuron cap: creatures above it now run on the
//! `forward_mse_scratch` kernel (runtime-sized storage activations), so the
//! pre-flight reports them as hostable. Only an unsupported squash function,
//! a shape mismatch, or an absurd neuron count now forces a CPU fallback.

use std::path::Path;

use rust_scorer::gpu::forward_mse_batched::GpuPrepareError;
use rust_scorer::multi_score::gpu_directory_compatible;

/// Write a forward-only creature with `hidden` TANH hidden neurons fully
/// connected between `num_inputs` inputs and `num_outputs` IDENTITY outputs.
/// Total neuron count is `num_inputs + hidden + num_outputs`.
fn write_creature(dir: &Path, name: &str, num_inputs: usize, num_outputs: usize, hidden: usize) {
    let mut neurons = Vec::new();
    for h in 0..hidden {
        neurons.push(format!(
            r#"{{"type":"hidden","uuid":"hidden-{h}","bias":0.01,"squash":"TANH"}}"#
        ));
    }
    for o in 0..num_outputs {
        neurons.push(format!(
            r#"{{"type":"output","uuid":"output-{o}","bias":0.0,"squash":"IDENTITY"}}"#
        ));
    }
    let mut synapses = Vec::new();
    for i in 0..num_inputs {
        for h in 0..hidden {
            synapses.push(format!(
                r#"{{"fromUUID":"input-{i}","toUUID":"hidden-{h}","weight":0.02}}"#
            ));
        }
    }
    for h in 0..hidden {
        for o in 0..num_outputs {
            synapses.push(format!(
                r#"{{"fromUUID":"hidden-{h}","toUUID":"output-{o}","weight":0.03}}"#
            ));
        }
    }
    let json = format!(
        r#"{{"input":{num_inputs},"output":{num_outputs},"forwardOnly":true,"semanticVersion":"4.0.0","neurons":[{}],"synapses":[{}]}}"#,
        neurons.join(","),
        synapses.join(","),
    );
    std::fs::write(dir.join(name), json).expect("write creature JSON");
}

/// Issue #182 lifted the 256-neuron cap: large creatures now run on the
/// `forward_mse_scratch` kernel, so the pre-flight must report them as
/// GPU-hostable instead of forcing a CPU fallback. (Before #182 this same
/// 302-neuron creature was rejected with a "256" shader-cap reason.)
#[test]
fn preflight_accepts_creature_above_private_cap() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    // 1 input + 300 hidden + 1 output = 302 neurons > 256 private-array cap.
    write_creature(tmp.path(), "big.json", 1, 1, 300);

    gpu_directory_compatible(tmp.path())
        .expect("a 302-neuron creature is now GPU-hostable via the scratch kernel");
}

/// A genuinely huge (4000+ hidden neuron) creature — the production scale the
/// issue targets — is also GPU-hostable after #182.
#[test]
fn preflight_accepts_large_production_scale_creature() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    // 8 inputs + 4000 hidden + 2 outputs = 4010 neurons.
    write_creature(tmp.path(), "huge.json", 8, 2, 4000);

    gpu_directory_compatible(tmp.path())
        .expect("a 4010-neuron creature is GPU-hostable via the scratch kernel");
}

#[test]
fn preflight_accepts_small_creature_set() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    write_creature(tmp.path(), "a.json", 1, 1, 2);
    write_creature(tmp.path(), "b.json", 1, 1, 3);

    gpu_directory_compatible(tmp.path()).expect("a tiny creature set must be GPU-hostable");
}

/// Issue #289 (C-GOOD-ERR): an incompatible set now returns the typed
/// [`GpuPrepareError`] directly instead of a stringly-typed `String`, so callers
/// can `match` on the specific incompatibility.
///
/// Issue #305 widened shader coverage to every point-wise activation, so
/// GAUSSIAN (once the canonical "unsupported" example here) is now hostable.
/// The remaining unsupported squashes are the *aggregate* functions, which
/// combine the individual weighted inputs rather than their sum — MEAN is used
/// here, and still yields `UnsupportedSquash`.
#[test]
fn preflight_returns_typed_error_for_unsupported_squash() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let json = r#"{"input":1,"output":1,"forwardOnly":true,"semanticVersion":"4.0.0",
        "neurons":[{"type":"output","uuid":"output-0","bias":0.0,"squash":"MEAN"}],
        "synapses":[{"fromUUID":"input-0","toUUID":"output-0","weight":0.5}]}"#;
    std::fs::write(tmp.path().join("mean.json"), json).expect("write creature JSON");

    let err = gpu_directory_compatible(tmp.path())
        .expect_err("a MEAN aggregate squash is not GPU-hostable");
    assert!(
        matches!(err, GpuPrepareError::UnsupportedSquash(_)),
        "expected UnsupportedSquash, got {err:?}"
    );
    // Display still yields a human-readable cause for logging.
    assert!(
        err.to_string().contains("squash"),
        "unexpected message: {err}"
    );
}

#[test]
fn preflight_defers_empty_directory_to_scoring_path() {
    // An empty directory is not a GPU-incompatibility — the pre-flight returns
    // Ok so the normal scoring path can surface the real "no creatures" error.
    let tmp = tempfile::tempdir().expect("create tempdir");
    assert!(
        gpu_directory_compatible(tmp.path()).is_ok(),
        "load failures must defer to the scoring path, not be reported as incompatibility"
    );
}
