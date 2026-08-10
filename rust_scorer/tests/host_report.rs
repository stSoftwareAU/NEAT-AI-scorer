//! Integration tests for the `--host-report` knob diagnostic (Issue #545).
//!
//! The report is the measurement rig the rest of the #544 retune chain cites,
//! so it must run **everywhere** — including the GPU-less CI runner and under
//! `--gpu off` — without initialising a `wgpu` adapter, and must emit one
//! stable JSON object a fleet operator can paste into an issue comment.

use std::process::Command;

use serde_json::Value;

/// Tuning knobs cleared on every invocation so an inherited value from the
/// developer's shell cannot flip a `source` assertion.
const TUNING_VARS: [&str; 5] = [
    "NEAT_SCORER_READ_BYTES",
    "NEAT_SCORER_ACTIVATION_THREADS",
    "NEAT_SCORER_FILE_THREADS",
    "NEAT_SCORER_GPU_SCRATCH_BYTES",
    "NEAT_SCORER_GPU",
];

/// Run the built binary with a hermetic environment and return
/// `(status_code, stdout, stderr)`.
fn run_scorer(args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rust_scorer"));
    for var in TUNING_VARS {
        cmd.env_remove(var);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.args(args).output().expect("run rust_scorer");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Parse stdout as exactly one JSON object (fails on trailing junk).
fn parse_report(stdout: &str) -> Value {
    let value: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("--host-report stdout must be one JSON object: {e}\n{stdout}"));
    assert!(
        value.is_object(),
        "--host-report must emit a JSON object, got: {value}"
    );
    value
}

/// Every knob entry the #544 retune sub-issues read.
const KNOB_KEYS: [&str; 5] = [
    "default_worker_count",
    "max_worker_count",
    "max_read_bytes",
    "default_training_read_bytes",
    "gpu_scratch_bytes",
];

fn assert_report_shape(report: &Value) {
    assert!(
        report["logical_cpus"].as_u64().is_some_and(|n| n >= 1),
        "logical_cpus must be a positive integer, got: {report}"
    );
    // Issue #546: the P-core count is always present and never exceeds the
    // logical count — a platform with no P/E split reports the two as equal
    // rather than dropping the field or reporting fewer.
    let logical = report["logical_cpus"].as_u64().unwrap_or_default();
    let performance = report["performance_cpus"].as_u64();
    assert!(
        performance.is_some_and(|n| n >= 1 && n <= logical),
        "performance_cpus must be in 1..=logical_cpus, got: {report}"
    );
    // `physical_ram_bytes` is `null` only where the platform probe is
    // unavailable; on every supported CI/fleet OS it is a positive integer.
    let ram = &report["physical_ram_bytes"];
    assert!(
        ram.is_null() || ram.as_u64().is_some_and(|n| n > 0),
        "physical_ram_bytes must be null or positive, got: {ram}"
    );
    assert!(
        report["record_bytes"].as_u64().is_some_and(|n| n > 0),
        "record_bytes must be a positive integer, got: {report}"
    );
    let knobs = report["knobs"]
        .as_object()
        .unwrap_or_else(|| panic!("report must carry a knobs object, got: {report}"));
    for key in KNOB_KEYS {
        let knob = knobs
            .get(key)
            .unwrap_or_else(|| panic!("knob '{key}' missing from report: {report}"));
        assert!(
            knob["value"].as_u64().is_some_and(|v| v > 0),
            "knob '{key}' must carry a positive value, got: {knob}"
        );
        let source = knob["source"].as_str().unwrap_or_default();
        assert!(
            source == "default" || source == "env",
            "knob '{key}' source must be 'default' or 'env', got: {knob}"
        );
    }
}

#[test]
fn host_report_runs_without_positional_args_and_emits_one_json_object() {
    let (code, stdout, stderr) = run_scorer(&["--host-report"], &[]);
    assert_eq!(code, 0, "--host-report must exit 0; stderr: {stderr}");
    let report = parse_report(&stdout);
    assert_report_shape(&report);
    for key in KNOB_KEYS {
        assert_eq!(
            report["knobs"][key]["source"], "default",
            "with no env override, knob '{key}' must report source=default"
        );
    }
}

#[test]
fn host_report_runs_with_gpu_off_without_initialising_an_adapter() {
    let (code, stdout, stderr) = run_scorer(&["--host-report", "--gpu", "off"], &[]);
    assert_eq!(
        code, 0,
        "--host-report --gpu off must exit 0; stderr: {stderr}"
    );
    assert_report_shape(&parse_report(&stdout));
}

/// `--gpu on` on a GPU-less host normally hard-errors; the report must still
/// run, because it never reaches adapter selection.
#[test]
fn host_report_never_aborts_even_under_gpu_on() {
    let (code, stdout, stderr) = run_scorer(&["--host-report", "--gpu", "on"], &[]);
    assert_eq!(
        code, 0,
        "--host-report --gpu on must still report; stderr: {stderr}"
    );
    assert_report_shape(&parse_report(&stdout));
}

#[test]
fn env_override_flips_the_matching_knob_source_to_env() {
    // 4 MiB is inside every host's `max_read_bytes` clamp, and a whole number
    // of the default 9848-byte record width is *not* required — the resolver
    // rounds down to the record multiple, so assert the rounded value.
    let (code, stdout, stderr) =
        run_scorer(&["--host-report"], &[("NEAT_SCORER_READ_BYTES", "4194304")]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let report = parse_report(&stdout);
    let knob = &report["knobs"]["default_training_read_bytes"];
    assert_eq!(
        knob["source"], "env",
        "an honoured NEAT_SCORER_READ_BYTES must report source=env, got: {knob}"
    );
    let record_bytes = report["record_bytes"].as_u64().expect("record_bytes");
    let value = knob["value"].as_u64().expect("value");
    assert_eq!(
        value,
        (4_194_304 / record_bytes) * record_bytes,
        "value must be the resolved (record-aligned) read target, got: {knob}"
    );
    // Untouched knobs keep reporting their built-in default.
    assert_eq!(report["knobs"]["gpu_scratch_bytes"]["source"], "default");

    let (code, stdout, _) = run_scorer(
        &["--host-report"],
        &[("NEAT_SCORER_GPU_SCRATCH_BYTES", "134217728")],
    );
    assert_eq!(code, 0);
    let report = parse_report(&stdout);
    assert_eq!(report["knobs"]["gpu_scratch_bytes"]["source"], "env");
    assert_eq!(
        report["knobs"]["gpu_scratch_bytes"]["value"],
        134_217_728u64
    );

    let (code, stdout, _) = run_scorer(
        &["--host-report"],
        &[("NEAT_SCORER_ACTIVATION_THREADS", "3")],
    );
    assert_eq!(code, 0);
    let report = parse_report(&stdout);
    assert_eq!(report["knobs"]["default_worker_count"]["source"], "env");
    assert_eq!(report["knobs"]["default_worker_count"]["value"], 3);
}

/// Issue #546: sensing performance cores must not weaken the override
/// contract — an honoured `NEAT_SCORER_ACTIVATION_THREADS` still resolves
/// inside `[1, max_worker_count]`, whatever the host's P/E split.
#[test]
fn worker_override_still_clamps_into_the_host_range() {
    for (raw, expect_floor) in [("999999", false), ("0", true)] {
        let (code, stdout, stderr) = run_scorer(
            &["--host-report"],
            &[("NEAT_SCORER_ACTIVATION_THREADS", raw)],
        );
        assert_eq!(code, 0, "stderr: {stderr}");
        let report = parse_report(&stdout);
        let ceiling = report["knobs"]["max_worker_count"]["value"]
            .as_u64()
            .expect("max_worker_count");
        let value = report["knobs"]["default_worker_count"]["value"]
            .as_u64()
            .expect("default_worker_count");
        assert!(
            (1..=ceiling).contains(&value),
            "override {raw} must clamp into [1, {ceiling}], got {value}"
        );
        if expect_floor {
            assert_eq!(value, 1, "override {raw} must clamp up to the floor");
        } else {
            assert_eq!(
                value, ceiling,
                "override {raw} must clamp down to the ceiling"
            );
        }
    }
}

/// A malformed override is *not* honoured (the shipped resolver warns and falls
/// back), so the report must keep saying `default` rather than claiming an
/// override that never took effect.
#[test]
fn malformed_env_override_still_reports_the_default_source() {
    let (code, stdout, stderr) =
        run_scorer(&["--host-report"], &[("NEAT_SCORER_READ_BYTES", "2MB")]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let report = parse_report(&stdout);
    assert_eq!(
        report["knobs"]["default_training_read_bytes"]["source"], "default",
        "an ignored malformed value must not be reported as an env override"
    );
    assert!(
        stderr.contains("ignoring invalid NEAT_SCORER_READ_BYTES"),
        "the malformed value must still be reported on stderr, got: {stderr}"
    );
}

/// The read default is record-size adaptive, so the report takes the width it
/// was resolved for and echoes it back.
#[test]
fn record_bytes_flag_drives_the_read_default() {
    let (code, stdout, stderr) = run_scorer(&["--host-report", "--record-bytes", "40"], &[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let small = parse_report(&stdout);
    assert_eq!(small["record_bytes"], 40);

    let (code, stdout, _) = run_scorer(&["--host-report", "--record-bytes", "9848"], &[]);
    assert_eq!(code, 0);
    let large = parse_report(&stdout);
    assert_eq!(large["record_bytes"], 9848);

    let small_read = small["knobs"]["default_training_read_bytes"]["value"]
        .as_u64()
        .expect("value");
    let large_read = large["knobs"]["default_training_read_bytes"]["value"]
        .as_u64()
        .expect("value");
    assert!(
        large_read > small_read,
        "production-width records must take a larger read chunk than 40-byte records \
         ({large_read} vs {small_read})"
    );
}

/// Input validation: a zero record width is rejected loudly rather than
/// silently clamped.
#[test]
fn zero_record_bytes_is_rejected() {
    let (code, _stdout, stderr) = run_scorer(&["--host-report", "--record-bytes", "0"], &[]);
    assert_ne!(code, 0, "--record-bytes 0 must fail loud");
    assert!(
        stderr.to_lowercase().contains("record"),
        "the error must name the offending flag, got: {stderr}"
    );
}

/// Two consecutive runs on the same host with the same environment must emit
/// byte-identical JSON — the report is pasted into issue comments and diffed
/// between fleet hosts.
#[test]
fn report_output_is_stable_across_runs() {
    let (_, first, _) = run_scorer(&["--host-report"], &[]);
    let (_, second, _) = run_scorer(&["--host-report"], &[]);
    assert_eq!(first, second, "--host-report output must be deterministic");
}
