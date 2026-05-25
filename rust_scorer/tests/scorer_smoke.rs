//! End-to-end smoke test for the `rust_scorer` binary.
//!
//! Runs the compiled binary against a tiny checked-in fixture (`tests/fixtures/`)
//! consisting of an identity creature and a 32-byte packed `f32` data file.
//!
//! Purpose: catch **API drift between `rust_scorer` and the path-dependency
//! `neat-core`** at PR time, instead of only when GRQ training fails downstream.
//! If the scorer fails to compile against the sibling `neat-core`, this test
//! never even runs (Cargo would fail earlier). If the scorer compiles but the
//! CLI contract / JSON output changes shape, this test fails loudly.
//!
//! Issue stSoftwareAU/NEAT-AI-scorer#11.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Resolve `tests/fixtures/<name>` relative to this crate.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Resolve a directory containing `<name>` for tests that need a data directory
/// (the scorer takes a directory of `.bin` files, not a single file). Copies
/// the fixture into a fresh temporary directory so concurrent test runs don't
/// trip over each other.
fn fixture_data_dir(bin_name: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let src = fixture(bin_name);
    let dst = tmp.path().join(bin_name);
    std::fs::copy(&src, &dst)
        .unwrap_or_else(|e| panic!("copy fixture {} -> {}: {e}", src.display(), dst.display()));
    tmp
}

/// Resolve a fresh temporary directory containing a pair of creature fixtures.
fn fixture_creatures_dir(names: &[&str]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("create tempdir");
    for name in names {
        let src = fixture(name);
        let dst = tmp.path().join(name);
        std::fs::copy(&src, &dst)
            .unwrap_or_else(|e| panic!("copy fixture {} -> {}: {e}", src.display(), dst.display()));
    }
    tmp
}

/// Run the `rust_scorer` binary against the fixture and assert the JSON output
/// matches the expected zero-error identity result.
#[test]
fn scorer_binary_runs_against_identity_fixture() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");

    let output = Command::new(bin)
        .arg(&creature)
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer binary");

    assert!(
        output.status.success(),
        "rust_scorer exited with status {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout is utf-8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON: {e}\n{stdout}"));

    // Identity network: every record contributes zero squared error.
    let error = parsed
        .get("error")
        .and_then(|v| v.as_f64())
        .expect("missing `error` in scorer JSON");
    assert!(error.abs() < 1e-6, "expected near-zero error, got {error}");

    // Score should be near 1.0 (perfect minus a tiny version penalty band).
    let score = parsed
        .get("score")
        .and_then(|v| v.as_f64())
        .expect("missing `score` in scorer JSON");
    assert!(
        score > 0.99 && score <= 1.0,
        "expected score close to 1.0, got {score}",
    );

    // Record count must match the fixture (4 records of 8 bytes each = 32 bytes).
    let record_count = parsed
        .get("recordCount")
        .and_then(|v| v.as_u64())
        .expect("missing `recordCount` in scorer JSON");
    assert_eq!(record_count, 4, "expected 4 records, got {record_count}");

    // Forward-only path was selected by the fixture creature.
    let forward_only = parsed
        .get("forwardOnly")
        .and_then(|v| v.as_bool())
        .expect("missing `forwardOnly` in scorer JSON");
    assert!(forward_only, "fixture sets forwardOnly: true");
}

/// Issue #38: when the read buffer is a whole-record multiple and `pending`
/// is empty at the start of a chunk callback, the scorer should skip the
/// memcpy through `pending` and score directly from the chunk slice. This
/// test exercises that fast path by:
///   - generating an identity-creature fixture with many records (more than
///     fit in one read buffer);
///   - forcing a small `NEAT_SCORER_READ_BYTES` so the read buffer is
///     exactly a whole-record multiple but several chunks are needed; and
///   - asserting the scoring result still reports zero error and the full
///     record count (i.e. records are not lost or double-counted).
#[test]
fn scorer_binary_record_aligned_multi_chunk_fast_path() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");

    // Identity creature: 1 input + 1 output = 8 bytes per record.
    // Build 1024 records (8 KiB) and force the read buffer to 32 bytes
    // (4 records) so the fast path is taken many times in sequence.
    let tmp = tempfile::tempdir().expect("create tempdir");
    let data_path = tmp.path().join("aligned_fast_path.bin");
    let mut file = std::fs::File::create(&data_path).expect("create data file");
    use std::io::Write as _;
    let n_records: usize = 1024;
    for i in 0..n_records {
        // Identity: input == output so squared error per record is zero.
        let v: f32 = (i as f32) * 1.0e-3;
        file.write_all(&v.to_le_bytes()).expect("write input");
        file.write_all(&v.to_le_bytes()).expect("write output");
    }
    drop(file);

    let output = Command::new(bin)
        .env("NEAT_SCORER_READ_BYTES", "32")
        .arg(&creature)
        .arg(tmp.path())
        .output()
        .expect("failed to spawn rust_scorer binary");

    assert!(
        output.status.success(),
        "rust_scorer exited with status {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout is utf-8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON: {e}\n{stdout}"));

    let error = parsed
        .get("error")
        .and_then(|v| v.as_f64())
        .expect("missing `error` in scorer JSON");
    assert!(error.abs() < 1e-6, "expected near-zero error, got {error}");

    let record_count = parsed
        .get("recordCount")
        .and_then(|v| v.as_u64())
        .expect("missing `recordCount` in scorer JSON");
    assert_eq!(
        record_count, n_records as u64,
        "expected {n_records} records, got {record_count}",
    );
}

/// Issue #80: `rust_scorer --gpu off` must produce JSON with the existing
/// schema plus a new `gpuBackend: "cpu-fallback"` field. The default mode
/// (no `--gpu` flag) must behave identically. This pins the JSON contract
/// for downstream callers parsing the scorer's output.
#[test]
fn scorer_binary_gpu_off_emits_cpu_fallback_label() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");

    // `--gpu off` explicit form.
    let output = Command::new(bin)
        .arg("--gpu")
        .arg("off")
        .arg(&creature)
        .arg(data_dir.path())
        .env_remove("NEAT_SCORER_GPU")
        .output()
        .expect("failed to spawn rust_scorer binary");
    assert!(
        output.status.success(),
        "rust_scorer --gpu off exited with status {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is utf-8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON: {e}\n{stdout}"));
    let gpu_backend = parsed
        .get("gpuBackend")
        .and_then(|v| v.as_str())
        .expect("missing `gpuBackend` in scorer JSON");
    assert_eq!(
        gpu_backend, "cpu-fallback",
        "`--gpu off` must always report cpu-fallback, got: {gpu_backend}",
    );

    // Issue #83: the default mode is now `auto`, but the single-creature
    // path stays on CPU (#81 negative result). The reported `gpuBackend`
    // reflects what actually ran, so single-creature mode must still
    // produce `cpu-fallback` regardless of host GPU presence.
    let default_out = Command::new(bin)
        .arg(&creature)
        .arg(data_dir.path())
        .env_remove("NEAT_SCORER_GPU")
        .output()
        .expect("failed to spawn rust_scorer binary");
    assert!(default_out.status.success());
    let default_json: serde_json::Value =
        serde_json::from_slice(&default_out.stdout).expect("default mode stdout is JSON");
    assert_eq!(
        default_json
            .get("gpuBackend")
            .and_then(|v| v.as_str())
            .unwrap(),
        "cpu-fallback",
        "single-creature path must report cpu-fallback under default Auto mode (Issue #83)",
    );

    // Existing fields must still appear with the same names — adding the new
    // key must not have shifted any other JSON field.
    for key in [
        "score",
        "error",
        "complexityPenalty",
        "recordCount",
        "hiddenNeurons",
        "synapseCount",
        "forwardOnly",
        "trainingReadBackend",
        "timeTaken",
    ] {
        assert!(
            parsed.get(key).is_some(),
            "expected existing key `{key}` in JSON, got:\n{stdout}",
        );
    }
}

/// Issue #83: `--gpu auto` on the single-creature path must always report
/// `cpu-fallback` because Issue #81 (single-creature GPU kernel) closed as a
/// negative result. The path is hard-coded to CPU regardless of which
/// adapter `wgpu` finds.
#[test]
fn scorer_binary_gpu_auto_single_creature_reports_cpu_fallback() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");

    let output = Command::new(bin)
        .arg("--gpu")
        .arg("auto")
        .arg(&creature)
        .arg(data_dir.path())
        .env_remove("NEAT_SCORER_GPU")
        .output()
        .expect("failed to spawn rust_scorer binary");
    assert!(
        output.status.success(),
        "rust_scorer --gpu auto exited with status {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(
        parsed.get("gpuBackend").and_then(|v| v.as_str()).unwrap(),
        "cpu-fallback",
        "single-creature path stays on CPU under Auto (#81 negative result, Issue #83)",
    );
}

/// Issue #80: `--gpu auto` must never panic, even on a host with no GPU.
/// The label can be any of the native backends (when a GPU is present) or
/// `cpu-fallback` (when none is available); we only assert it parses.
#[test]
fn scorer_binary_gpu_auto_runs_without_panic() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");

    let output = Command::new(bin)
        .arg("--gpu")
        .arg("auto")
        .arg(&creature)
        .arg(data_dir.path())
        .env_remove("NEAT_SCORER_GPU")
        .output()
        .expect("failed to spawn rust_scorer binary");
    assert!(
        output.status.success(),
        "rust_scorer --gpu auto exited with status {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is utf-8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON: {e}\n{stdout}"));
    let gpu_backend = parsed
        .get("gpuBackend")
        .and_then(|v| v.as_str())
        .expect("missing `gpuBackend` in scorer JSON");
    assert!(
        matches!(
            gpu_backend,
            "cpu-fallback" | "metal" | "vulkan" | "dx12" | "gl"
        ),
        "unexpected gpuBackend label: {gpu_backend}",
    );
}

/// Issue #80: `NEAT_SCORER_GPU=off` (env var, no flag) must take effect when
/// the CLI flag is absent. The CLI flag must override the env var.
#[test]
fn scorer_binary_gpu_env_var_is_honoured() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");

    // Env var only.
    let output = Command::new(bin)
        .env("NEAT_SCORER_GPU", "off")
        .arg(&creature)
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer binary");
    assert!(output.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(
        parsed.get("gpuBackend").and_then(|v| v.as_str()).unwrap(),
        "cpu-fallback",
    );

    // CLI overrides env var: `--gpu off` wins even when env says `auto`.
    let output = Command::new(bin)
        .env("NEAT_SCORER_GPU", "auto")
        .arg("--gpu")
        .arg("off")
        .arg(&creature)
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer binary");
    assert!(output.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(
        parsed.get("gpuBackend").and_then(|v| v.as_str()).unwrap(),
        "cpu-fallback",
        "CLI `--gpu off` must override `NEAT_SCORER_GPU=auto`",
    );
}

/// Issue #80: a malformed `NEAT_SCORER_GPU` value must produce a non-zero
/// exit with a clear error message rather than silently falling back.
#[test]
fn scorer_binary_gpu_env_var_rejects_garbage() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");

    let output = Command::new(bin)
        .env("NEAT_SCORER_GPU", "yolo")
        .arg(&creature)
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer binary");
    assert!(
        !output.status.success(),
        "expected non-zero exit for malformed NEAT_SCORER_GPU",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("yolo") || stderr.contains("auto"),
        "stderr should describe the invalid GPU mode, got: {stderr}",
    );
}

/// Sanity check: missing creature file produces a non-zero exit and a clear error.
/// Also serves as a regression for the CLI contract (positional args).
#[test]
fn scorer_binary_fails_when_creature_missing() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let data_dir = fixture_data_dir("identity_data.bin");

    let output = Command::new(bin)
        .arg("/definitely/not/a/real/path/creature.json")
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer binary");

    assert!(
        !output.status.success(),
        "expected non-zero exit when creature file missing",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Failed to read creature file"),
        "expected diagnostic about missing creature file, got: {stderr}",
    );
}

/// End-to-end check for `--creature-stdin` (issue #15): piping the creature
/// JSON over stdin must produce the same numeric result as the default file
/// mode, without needing a temp file on disk.
#[test]
fn scorer_binary_accepts_creature_via_stdin() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature_path = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");
    let creature_json =
        std::fs::read_to_string(&creature_path).expect("read identity_creature.json fixture");

    let mut child = Command::new(bin)
        .arg("--creature-stdin")
        .arg(data_dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn rust_scorer binary");

    child
        .stdin
        .as_mut()
        .expect("child stdin")
        .write_all(creature_json.as_bytes())
        .expect("write creature json to stdin");
    // Drop stdin so the child sees EOF and proceeds.
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("failed to wait on rust_scorer");

    assert!(
        output.status.success(),
        "rust_scorer --creature-stdin exited with status {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout is utf-8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON: {e}\n{stdout}"));

    let error = parsed
        .get("error")
        .and_then(|v| v.as_f64())
        .expect("missing `error` in scorer JSON");
    assert!(error.abs() < 1e-6, "expected near-zero error, got {error}");

    let record_count = parsed
        .get("recordCount")
        .and_then(|v| v.as_u64())
        .expect("missing `recordCount` in scorer JSON");
    assert_eq!(record_count, 4, "expected 4 records, got {record_count}");
}

/// Directory mode: score multiple creatures in one training-data scan, and
/// ensure each keyed result is bit-identical to single-creature mode.
#[test]
fn scorer_binary_directory_mode_matches_single_mode_results() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let data_dir = fixture_data_dir("identity_data.bin");
    let creatures_dir = fixture_creatures_dir(&[
        "identity_creature_uuid_a.json",
        "identity_creature_uuid_b.json",
    ]);

    let out_multi = Command::new(bin)
        .arg(creatures_dir.path())
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer in directory mode");
    assert!(
        out_multi.status.success(),
        "directory mode exited with status {:?}\nstderr:\n{}",
        out_multi.status.code(),
        String::from_utf8_lossy(&out_multi.stderr),
    );

    let multi_stdout = String::from_utf8(out_multi.stdout).expect("stdout is utf-8");
    let multi: serde_json::Value = serde_json::from_str(&multi_stdout)
        .unwrap_or_else(|e| panic!("directory mode stdout is not JSON: {e}\n{multi_stdout}"));

    let creatures = [
        ("identity_creature_uuid_a", "identity_creature_uuid_a.json"),
        ("identity_creature_uuid_b", "identity_creature_uuid_b.json"),
    ];
    for (key, fixture_name) in creatures {
        let single = Command::new(bin)
            .arg(fixture(fixture_name))
            .arg(data_dir.path())
            .output()
            .expect("failed to spawn rust_scorer in single mode");
        assert!(
            single.status.success(),
            "single mode exited with status {:?}\nstderr:\n{}",
            single.status.code(),
            String::from_utf8_lossy(&single.stderr),
        );
        let single_stdout = String::from_utf8(single.stdout).expect("stdout is utf-8");
        let single_json: serde_json::Value = serde_json::from_str(&single_stdout)
            .unwrap_or_else(|e| panic!("single mode stdout is not JSON: {e}\n{single_stdout}"));

        let from_multi = multi
            .get(key)
            .unwrap_or_else(|| panic!("missing key '{key}' in directory output"));

        // With deeper parallel chunk reductions, floating-point summation order can differ
        // by a tiny epsilon while remaining numerically equivalent.
        let multi_error = from_multi
            .get("error")
            .and_then(|v| v.as_f64())
            .expect("multi missing error");
        let single_error = single_json
            .get("error")
            .and_then(|v| v.as_f64())
            .expect("single missing error");
        assert!(
            (multi_error - single_error).abs() < 1e-9,
            "error mismatch too large: multi={multi_error}, single={single_error}",
        );

        let multi_score = from_multi
            .get("score")
            .and_then(|v| v.as_f64())
            .expect("multi missing score");
        let single_score = single_json
            .get("score")
            .and_then(|v| v.as_f64())
            .expect("single missing score");
        assert!(
            (multi_score - single_score).abs() < 1e-9,
            "score mismatch too large: multi={multi_score}, single={single_score}",
        );
        assert_eq!(
            from_multi.get("recordCount"),
            single_json.get("recordCount")
        );
    }
}

/// Issue #120: `--cost MSE` must run identically to the current default
/// (no `--cost` flag) — score, error, and record count must match exactly.
/// Pins the "MSE preserves historical behaviour" acceptance criterion.
#[test]
fn scorer_binary_cost_mse_matches_default() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");

    let baseline = Command::new(bin)
        .arg(&creature)
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer (baseline)");
    assert!(
        baseline.status.success(),
        "baseline exited with status {:?}\nstderr:\n{}",
        baseline.status.code(),
        String::from_utf8_lossy(&baseline.stderr),
    );
    let baseline_stdout = String::from_utf8(baseline.stdout).expect("stdout is utf-8");
    let baseline_json: serde_json::Value =
        serde_json::from_str(&baseline_stdout).expect("baseline stdout is JSON");

    let with_mse = Command::new(bin)
        .arg("--cost")
        .arg("MSE")
        .arg(&creature)
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer (--cost MSE)");
    assert!(
        with_mse.status.success(),
        "--cost MSE exited with status {:?}\nstderr:\n{}",
        with_mse.status.code(),
        String::from_utf8_lossy(&with_mse.stderr),
    );
    let mse_stdout = String::from_utf8(with_mse.stdout).expect("stdout is utf-8");
    let mse_json: serde_json::Value =
        serde_json::from_str(&mse_stdout).expect("--cost MSE stdout is JSON");

    assert_eq!(
        baseline_json.get("error"),
        mse_json.get("error"),
        "--cost MSE must match the default scoring path"
    );
    assert_eq!(
        baseline_json.get("score"),
        mse_json.get("score"),
        "--cost MSE must match the default scoring path"
    );
    assert_eq!(
        baseline_json.get("recordCount"),
        mse_json.get("recordCount"),
    );
}

/// Issue #121 update: `--cost MAE` now dispatches the real MAE helper
/// (previously this asserted MAE silently computed MSE while dispatch was
/// pending). The fixture is an identity creature whose predictions exactly
/// match the targets, so both MSE and MAE produce 0.0 error — that gives
/// us a stable numerical equality without needing different fixtures per
/// cost, while still proving the binary accepts and runs `--cost MAE` end
/// to end. The `costName` JSON field must echo `"MAE"` so downstream
/// callers can confirm dispatch.
#[test]
fn scorer_binary_cost_mae_runs_through_dispatch() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");

    let baseline = Command::new(bin)
        .arg("--cost")
        .arg("MSE")
        .arg(&creature)
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer (--cost MSE)");
    assert!(baseline.status.success());
    let baseline_json: serde_json::Value =
        serde_json::from_slice(&baseline.stdout).expect("baseline stdout is JSON");
    assert_eq!(
        baseline_json.get("costName").and_then(|v| v.as_str()),
        Some("MSE"),
        "MSE run must report costName=MSE"
    );

    let with_mae = Command::new(bin)
        .arg("--cost")
        .arg("MAE")
        .arg(&creature)
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer (--cost MAE)");
    assert!(
        with_mae.status.success(),
        "--cost MAE exited with status {:?}\nstderr:\n{}",
        with_mae.status.code(),
        String::from_utf8_lossy(&with_mae.stderr),
    );
    let mae_json: serde_json::Value =
        serde_json::from_slice(&with_mae.stdout).expect("--cost MAE stdout is JSON");

    // Identity fixture: every prediction equals its target so both costs
    // evaluate to exactly 0.0, regardless of which dispatch path ran.
    assert_eq!(baseline_json.get("error"), mae_json.get("error"));
    assert_eq!(baseline_json.get("score"), mae_json.get("score"));
    assert_eq!(
        mae_json.get("costName").and_then(|v| v.as_str()),
        Some("MAE"),
        "MAE run must report costName=MAE"
    );
}

/// Issue #121 update: every regression-friendly cost (MSE, MAE, MAPE,
/// MSLE, HINGE) must now run end-to-end through real dispatch and report
/// its name in `costName`. `CROSS_ENTROPY` is excluded here because the
/// shared identity fixture contains negative targets, which is not a
/// valid probability — neat-core happily computes a (negative) CE there
/// but the scorer's `error >= 0` invariant in `calculate_score` then
/// fails. A dedicated `[0, 1]`-target fixture exercises CE separately
/// below. Issue #134: `CATEGORICAL_ERROR` is also exercised in its own
/// dedicated test (the shared 1-output fixture is degenerate for
/// argmax, so a focused test reads better than wedging it into the
/// shared sweep).
#[test]
fn scorer_binary_accepts_every_dispatchable_built_in_cost_name() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");

    for name in ["MSE", "MAE", "MAPE", "MSLE", "HINGE"] {
        let out = Command::new(bin)
            .arg("--cost")
            .arg(name)
            .arg(&creature)
            .arg(data_dir.path())
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn rust_scorer for cost {name}: {e}"));
        assert!(
            out.status.success(),
            "--cost {name} exited with status {:?}\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr),
        );
        let json: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout must be JSON");
        assert_eq!(
            json.get("costName").and_then(|v| v.as_str()),
            Some(name),
            "--cost {name} must echo costName={name}"
        );
    }
}

/// Issue #121: `--cost CROSS_ENTROPY` must run end-to-end against a
/// `[0, 1]`-probability fixture (where the loss is well-defined) and
/// report `costName=CROSS_ENTROPY`. This is the dedicated CE counterpart
/// to the regression-friendly dispatchable-costs test above, exercising
/// the LOGISTIC squash so outputs stay in `(0, 1)`.
#[test]
fn scorer_binary_cost_cross_entropy_runs_on_probabilistic_fixture() {
    use std::io::Write;
    use tempfile::TempDir;

    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let tmp = TempDir::new().expect("create tempdir");

    // Logistic-output creature so predictions stay in (0, 1).
    let creature_json = r#"{
        "input": 1,
        "output": 1,
        "forwardOnly": true,
        "semanticVersion": "4.0.0",
        "neurons": [
            {"type": "output", "uuid": "output-0", "bias": 0.0, "squash": "LOGISTIC"}
        ],
        "synapses": [
            {"fromUUID": "input-0", "toUUID": "output-0", "weight": 1.0}
        ]
    }"#;
    let creature_path = tmp.path().join("creature.json");
    std::fs::write(&creature_path, creature_json).expect("write creature.json");

    let data_dir = tmp.path().join("data");
    std::fs::create_dir(&data_dir).expect("create data dir");
    let mut bin_file = std::fs::File::create(data_dir.join("0.bin")).expect("create 0.bin");
    // [inputs..., targets...] packed records, targets ∈ [0, 1].
    let records: &[(f32, f32)] = &[(0.5, 0.6), (1.0, 0.9), (-1.0, 0.1), (2.0, 0.8)];
    for (input, target) in records {
        bin_file.write_all(&input.to_le_bytes()).unwrap();
        bin_file.write_all(&target.to_le_bytes()).unwrap();
    }
    drop(bin_file);

    let out = Command::new(bin)
        .arg("--cost")
        .arg("CROSS_ENTROPY")
        .arg(&creature_path)
        .arg(&data_dir)
        .output()
        .expect("failed to spawn rust_scorer (--cost CROSS_ENTROPY)");
    assert!(
        out.status.success(),
        "--cost CROSS_ENTROPY must succeed on a [0,1] fixture, got status {:?}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout must be JSON");
    assert_eq!(
        json.get("costName").and_then(|v| v.as_str()),
        Some("CROSS_ENTROPY"),
        "CE run must echo costName=CROSS_ENTROPY"
    );
    let error = json
        .get("error")
        .and_then(|v| v.as_f64())
        .expect("error must be a number");
    assert!(
        error.is_finite() && error >= 0.0,
        "CE error must be non-negative and finite, got {error}"
    );
}

/// Issue #134: `--cost CATEGORICAL_ERROR` must run end-to-end now that
/// `categorical_error_sum_batch_packed` has landed upstream
/// (`NEAT-AI-core#88`). The shared identity fixture has matching
/// `input == target` so argmax(input) == argmax(target) on every record
/// and the misclassification count is exactly 0 — a clean success path
/// for both the binary's exit status and the JSON contract.
#[test]
fn scorer_binary_categorical_error_runs_after_upstream_helper_landed() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");

    let out = Command::new(bin)
        .arg("--cost")
        .arg("CATEGORICAL_ERROR")
        .arg(&creature)
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer (--cost CATEGORICAL_ERROR)");
    assert!(
        out.status.success(),
        "--cost CATEGORICAL_ERROR must succeed after NEAT-AI-core#88, got status {:?}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout must be JSON");
    assert_eq!(
        json.get("costName").and_then(|v| v.as_str()),
        Some("CATEGORICAL_ERROR"),
        "CATEGORICAL_ERROR run must echo costName=CATEGORICAL_ERROR"
    );
    let error = json
        .get("error")
        .and_then(|v| v.as_f64())
        .expect("error must be a number");
    assert!(
        error.is_finite() && error >= 0.0,
        "categorical error must be non-negative and finite, got {error}"
    );
}

/// Issue #121: `--gpu on --cost MAE` must hard-error because the
/// `forward_mse_batched` kernel has no MAE implementation. The error
/// message must name the cost so the user can recover by switching mode
/// or cost.
#[test]
fn scorer_binary_gpu_on_with_non_mse_cost_errors() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");

    let out = Command::new(bin)
        .arg("--gpu")
        .arg("on")
        .arg("--cost")
        .arg("MAE")
        .arg(&creature)
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer (--gpu on --cost MAE)");
    assert!(
        !out.status.success(),
        "--gpu on --cost MAE must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("MAE") && stderr.contains("GPU"),
        "stderr must mention MAE and GPU, got: {stderr}"
    );
}

/// Issue #121: `--gpu auto --cost MAE` must complete successfully on
/// CPU. Whether or not a GPU is available, an unsupported cost forces
/// the silent fallback path — `gpuBackend` should report `cpu-fallback`
/// because no GPU kernel ran.
#[test]
fn scorer_binary_gpu_auto_with_non_mse_cost_runs_on_cpu() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");

    let out = Command::new(bin)
        .arg("--gpu")
        .arg("auto")
        .arg("--cost")
        .arg("MAE")
        .arg(&creature)
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer (--gpu auto --cost MAE)");
    assert!(
        out.status.success(),
        "--gpu auto --cost MAE must succeed via CPU fallback, got status {:?}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout must be JSON");
    assert_eq!(
        json.get("costName").and_then(|v| v.as_str()),
        Some("MAE"),
        "auto/MAE run must echo costName=MAE"
    );
    // The single-creature path always reports `cpu-fallback` in `gpuBackend`
    // because Issue #81 closed without a single-creature GPU kernel — that
    // is independent of `--cost`, but lock it in here as part of the
    // contract anyway.
    assert_eq!(
        json.get("gpuBackend").and_then(|v| v.as_str()),
        Some("cpu-fallback"),
        "non-MSE cost must run on CPU; gpuBackend should be cpu-fallback"
    );
}

/// Issue #120: an unknown `--cost` value must produce a non-zero exit
/// and a stderr message naming the supported set. Pins the "hard-error
/// on unknown cost" acceptance criterion.
#[test]
fn scorer_binary_rejects_unknown_cost() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");

    let out = Command::new(bin)
        .arg("--cost")
        .arg("FOO")
        .arg(&creature)
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer");
    assert!(
        !out.status.success(),
        "--cost FOO must exit non-zero, got status {:?}",
        out.status.code(),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("FOO"),
        "stderr must echo bad value, got: {stderr}"
    );
    // clap renders "[possible values: MSE, MAE, ...]"; assert the
    // supported set is mentioned.
    assert!(
        stderr.contains("MSE") && stderr.contains("CROSS_ENTROPY"),
        "stderr must list supported costs, got: {stderr}"
    );
}

/// Issue #120: `--help` must document the `--cost` flag and its supported
/// values so callers can discover them without reading source.
#[test]
fn scorer_binary_help_lists_cost_flag_and_values() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let out = Command::new(bin)
        .arg("--help")
        .output()
        .expect("failed to spawn rust_scorer --help");
    assert!(
        out.status.success(),
        "--help exited with status {:?}",
        out.status.code()
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--cost"),
        "--help must mention --cost, got:\n{stdout}"
    );
    for name in [
        "MSE",
        "MAE",
        "MAPE",
        "MSLE",
        "HINGE",
        "CROSS_ENTROPY",
        "CATEGORICAL_ERROR",
    ] {
        assert!(
            stdout.contains(name),
            "--help must list supported cost '{name}', got:\n{stdout}"
        );
    }
}

/// Issue #120 explicitly forbids a `NEAT_SCORER_COST` env-var override
/// (KISS — the CLI flag is the only knob). Pin that contract by setting
/// the env var to an invalid value and asserting the binary still runs
/// using the default (`MSE`) — i.e. the env var is ignored.
#[test]
fn scorer_binary_ignores_neat_scorer_cost_env_var() {
    let bin = env!("CARGO_BIN_EXE_rust_scorer");
    let creature = fixture("identity_creature.json");
    let data_dir = fixture_data_dir("identity_data.bin");

    let out = Command::new(bin)
        // If this env var had any effect, "FOO" would either be picked up
        // (impossible — not a valid CostKind) or trigger a parse error.
        // The binary must ignore it and use the default.
        .env("NEAT_SCORER_COST", "FOO")
        .arg(&creature)
        .arg(data_dir.path())
        .output()
        .expect("failed to spawn rust_scorer");
    assert!(
        out.status.success(),
        "binary must ignore NEAT_SCORER_COST, got status {:?}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
}
