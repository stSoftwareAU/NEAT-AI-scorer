//! Issue #180 — CPU-only GPU compatibility pre-flight for directory mode.
//!
//! `gpu_directory_compatible` decides — without creating a `wgpu` device —
//! whether the `forward_mse_batched` kernel can host every creature in a
//! directory. `main.rs` consults it under `--gpu auto` so a creature set above
//! the 256-neuron shader cap routes straight to the CPU pipeline instead of
//! spinning up (and tearing down) a GPU context, which previously surfaced to
//! batch callers as `exit 158` / truncated JSON.

use std::path::Path;

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

#[test]
fn preflight_rejects_creature_above_shader_cap() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    // 1 input + 300 hidden + 1 output = 302 neurons > 256-neuron shader cap.
    write_creature(tmp.path(), "big.json", 1, 1, 300);

    let err = gpu_directory_compatible(tmp.path())
        .expect_err("a 302-neuron creature must be rejected by the GPU pre-flight");
    assert!(
        err.contains("256"),
        "expected the shader-cap reason, got: {err}"
    );
    assert!(
        err.contains("neurons"),
        "expected the neuron-count reason, got: {err}"
    );
}

#[test]
fn preflight_accepts_small_creature_set() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    write_creature(tmp.path(), "a.json", 1, 1, 2);
    write_creature(tmp.path(), "b.json", 1, 1, 3);

    gpu_directory_compatible(tmp.path()).expect("a tiny creature set must be GPU-hostable");
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
