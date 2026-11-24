//! Behavioural contract for the opt-in experiment command.
use std::process::Command;

use rust_scorer::fixture_json::{creature_envelope, neuron_json, synapse_json};
use tempfile::TempDir;

fn fixture() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("candidates")).unwrap();
    std::fs::create_dir(dir.path().join("data")).unwrap();
    for i in 0..12 {
        let json = creature_envelope(
            1,
            1,
            &[neuron_json(
                "output",
                "output-0",
                i as f64 / 10.0,
                "IDENTITY",
            )],
            &[synapse_json("input-0", "output-0", 1.0)],
        );
        std::fs::write(dir.path().join(format!("candidates/c{i:02}.json")), json).unwrap();
    }
    let bytes: Vec<u8> = (0..200)
        .flat_map(|i| {
            let x = i as f32 / 200.0;
            [x, x + 0.3].into_iter().flat_map(f32::to_le_bytes)
        })
        .collect();
    std::fs::write(dir.path().join("data/a.bin"), bytes).unwrap();
    dir
}

fn command(dir: &TempDir) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_halving_bench"));
    c.arg(dir.path().join("candidates"))
        .arg(dir.path().join("data"));
    c.env("NEAT_SCORER_ACTIVATION_THREADS", "1");
    c.stdin(std::process::Stdio::null());
    c
}

#[test]
fn benchmark_reports_four_controls_and_authoritative_rejection_audits() {
    let dir = fixture();
    let before = std::fs::read(dir.path().join("candidates/c03.json")).unwrap();
    let out = command(&dir).arg("--cost").arg("RMSE").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema"], "neat-halving-observation/1");
    assert_eq!(v["records"], 200);
    assert_eq!(v["authoritativeRecordEvaluations"], 2400);
    let arms = v["arms"].as_array().unwrap();
    assert_eq!(arms.len(), 4);
    for arm in arms {
        assert_eq!(arm["audit"]["fullWinner"], 3);
        assert_eq!(arm["audit"]["regret"], 0.0);
        assert_eq!(arm["outcome"]["selected"], 3);
        assert!(arm["maxRelativeDeviation"].as_f64().unwrap() <= 1e-6);
        assert_eq!(
            arm["rejected"].as_array().unwrap().len(),
            arm["outcome"]["eliminatedAt"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| !v.is_null())
                .count()
        );
    }
    assert_eq!(
        std::fs::read(dir.path().join("candidates/c03.json")).unwrap(),
        before
    );
}

#[test]
fn benchmark_refuses_limits_and_malformed_data_loudly() {
    let dir = fixture();
    let out = command(&dir)
        .arg("--max-corpus-bytes")
        .arg("16")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "failed");
    assert!(v["error"].as_str().unwrap().contains("corpus byte limit"));
    // With the limit lifted, this input reaches scoring (the positive control).
    assert!(command(&dir).output().unwrap().status.success());
    std::fs::write(dir.path().join("data/a.bin"), [0_u8; 9]).unwrap();
    let out = command(&dir).output().unwrap();
    assert!(!out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["error"].as_str().unwrap().contains("record"));
}

#[test]
fn benchmark_refuses_bad_orders_and_unsafe_stride_phases() {
    let dir = fixture();
    for (flag, value, error) in [
        ("--order", "full,full,nested,cached", "permutation"),
        (
            "--seed",
            "18446744073709551615",
            "seed must be in [0, 2^32-1]",
        ),
        (
            "--max-record-evaluations",
            "1",
            "record evaluation limit exceeded",
        ),
    ] {
        let out = command(&dir).args([flag, value]).output().unwrap();
        assert!(!out.status.success());
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(v["error"].as_str().unwrap().contains(error), "{v}");
    }
    assert!(
        command(&dir)
            .args(["--order", "cached,nested,stride,full", "--seed", "17"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn rejection_audit_finds_a_winner_hidden_from_the_early_stratum() {
    let dir = fixture();
    for i in 2..12 {
        std::fs::remove_file(dir.path().join(format!("candidates/c{i:02}.json"))).unwrap();
    }
    for i in 0..2 {
        let json = creature_envelope(
            1,
            1,
            &[neuron_json("output", "output-0", i as f64, "IDENTITY")],
            &[synapse_json("input-0", "output-0", 0.0)],
        );
        std::fs::write(dir.path().join(format!("candidates/c{i:02}.json")), json).unwrap();
    }
    // For seed 0 and 100 records, the first nested rung observes IDs 0 and 64.
    // Those two targets mislead selection; the other 98 favour candidate 1.
    let data: Vec<u8> = (0..100)
        .flat_map(|i| {
            [0.0_f32, if i == 0 || i == 64 { 0.0 } else { 1.0 }]
                .into_iter()
                .flat_map(f32::to_le_bytes)
        })
        .collect();
    std::fs::write(dir.path().join("data/a.bin"), data).unwrap();
    let out = command(&dir).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let cached = v["arms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["outcome"]["arm"] == "cachedHalving")
        .unwrap();
    assert_eq!(cached["outcome"]["selected"], 0);
    assert_eq!(cached["audit"]["fullWinner"], 1);
    assert_eq!(cached["audit"]["lostFullWinner"], true);
    assert_eq!(cached["audit"]["rejectedBetterThanSelected"], 1);
    assert!((cached["audit"]["regret"].as_f64().unwrap() - 0.96).abs() < 1e-12);
    assert_eq!(cached["rejected"][0]["candidate"], 1);
    assert_eq!(cached["rejected"][0]["fullError"], 0.02);
}

#[test]
fn frozen_driver_runs_real_worker_and_rejects_changed_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("fixture");
    let generated = Command::new(env!("CARGO_BIN_EXE_halving_bench"))
        .arg("--generate-fixture")
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let mut config: serde_json::Value = serde_json::from_slice(&generated.stdout).unwrap();
    config["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|c| c["id"].as_str().unwrap().ends_with("-12"));
    config["seeds"] = serde_json::json!([17]);
    config["repetitions"] = serde_json::json!(4);
    let config_path = dir.path().join("config.json");
    let manifest = dir.path().join("manifest.json");
    let results = dir.path().join("results.jsonl");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let driver =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/bench-halving.py");
    let python = || {
        let mut c = Command::new("python3");
        c.arg("-B").arg(&driver).stdin(std::process::Stdio::null());
        c.env("NEAT_SCORER_ACTIVATION_THREADS", "1");
        c
    };
    let frozen = python()
        .arg("prepare")
        .arg("--config")
        .arg(&config_path)
        .arg("--manifest")
        .arg(&manifest)
        .arg("--binary")
        .arg(env!("CARGO_BIN_EXE_halving_bench"))
        .output()
        .unwrap();
    assert!(
        frozen.status.success(),
        "{}",
        String::from_utf8_lossy(&frozen.stderr)
    );
    for field in ["creaturesDir", "dataDir"] {
        let mut overlapping = config.clone();
        overlapping["cases"][1][field] = overlapping["cases"][0][field].clone();
        let overlap_path = dir.path().join(format!("overlap-{field}.json"));
        let overlap_manifest = dir.path().join(format!("overlap-{field}-manifest.json"));
        std::fs::write(&overlap_path, serde_json::to_vec(&overlapping).unwrap()).unwrap();
        let refused = python()
            .arg("prepare")
            .arg("--config")
            .arg(&overlap_path)
            .arg("--manifest")
            .arg(&overlap_manifest)
            .arg("--binary")
            .arg(env!("CARGO_BIN_EXE_halving_bench"))
            .output()
            .unwrap();
        assert!(!refused.status.success());
        assert!(String::from_utf8_lossy(&refused.stderr).contains("hashes overlap"));
        assert!(!overlap_manifest.exists());
    }
    let run = python()
        .arg("run")
        .arg("--manifest")
        .arg(&manifest)
        .arg("--results")
        .arg(&results)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&results)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let summary = lines.last().unwrap();
    assert_eq!(summary["coverage"]["successfulTrials"], 8);
    assert_eq!(summary["coverage"]["failedTrials"], 0);
    assert_eq!(
        summary["verdict"],
        "inconclusive-no-representative-validation"
    );
    assert_eq!(summary["comparisons"].as_array().unwrap().len(), 2);
    #[cfg(unix)]
    {
        let mut changed_binary: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        changed_binary["binary"] = serde_json::json!("/bin/sh");
        let changed_manifest = dir.path().join("changed-binary.json");
        std::fs::write(
            &changed_manifest,
            serde_json::to_vec(&changed_binary).unwrap(),
        )
        .unwrap();
        let refused = python()
            .arg("run")
            .arg("--manifest")
            .arg(&changed_manifest)
            .arg("--results")
            .arg(dir.path().join("changed-binary-results.jsonl"))
            .output()
            .unwrap();
        assert!(!refused.status.success());
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("source or binary changed"),
            "{}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
    // An actual native limit failure stays failed in the aggregate, with its cause.
    let mut limited = config.clone();
    limited["limits"]["maxRecordEvaluations"] = serde_json::json!(1);
    let limited_config = dir.path().join("limited.json");
    let limited_manifest = dir.path().join("limited-manifest.json");
    let limited_results = dir.path().join("limited-results.jsonl");
    std::fs::write(&limited_config, serde_json::to_vec(&limited).unwrap()).unwrap();
    let prepared = python()
        .arg("prepare")
        .arg("--config")
        .arg(&limited_config)
        .arg("--manifest")
        .arg(&limited_manifest)
        .arg("--binary")
        .arg(env!("CARGO_BIN_EXE_halving_bench"))
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let failed = python()
        .arg("run")
        .arg("--manifest")
        .arg(&limited_manifest)
        .arg("--results")
        .arg(&limited_results)
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let failed_lines: Vec<serde_json::Value> = std::fs::read_to_string(&limited_results)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(failed_lines.last().unwrap()["coverage"]["failedTrials"], 8);
    assert_eq!(
        failed_lines.last().unwrap()["verdict"],
        "failed-or-incomplete"
    );
    assert!(
        failed_lines[1]["error"]
            .as_str()
            .unwrap()
            .contains("record evaluation limit exceeded")
    );
    std::fs::write(root.join("validation-12/data/0.bin"), [0_u8; 80]).unwrap();
    let changed = python()
        .arg("run")
        .arg("--manifest")
        .arg(&manifest)
        .arg("--results")
        .arg(dir.path().join("changed.jsonl"))
        .output()
        .unwrap();
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("frozen data changed"));
    assert!(!dir.path().join("changed.jsonl").exists());
}

/// The Python driver's protocol and statistics contracts (counterbalanced arm
/// orders, the seed-clustered interval and the config refusals in
/// `tests/experiments/test_halving_driver.py`) run inside `cargo test`, so the
/// normal gate — `./quality.sh` and CI's `cargo test` — fails when they break.
#[test]
fn python_driver_contract_tests_pass_in_the_cargo_gate() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let out = Command::new("python3")
        .args([
            "-B",
            "-m",
            "unittest",
            "discover",
            "-s",
            "tests/experiments",
            "-v",
        ])
        .current_dir(&root)
        .output()
        .expect("python3 must be available to run the driver contract tests");
    let log = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "driver contract tests failed:\n{log}");
    let ran = log
        .lines()
        .find_map(|l| {
            l.strip_prefix("Ran ")?
                .split_whitespace()
                .next()?
                .parse::<usize>()
                .ok()
        })
        .unwrap_or(0);
    assert!(
        ran >= 4,
        "expected the driver contract tests to run, got {ran}:\n{log}"
    );
}

#[test]
fn near_clone_fixture_writes_offspring_of_one_parent_and_scores_authoritatively() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let generated = Command::new(env!("CARGO_BIN_EXE_halving_bench"))
        .args(["--generate-fixture"])
        .arg(&root)
        .args(["--fixture-profile", "near-clone"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let config: serde_json::Value = serde_json::from_slice(&generated.stdout).unwrap();
    let cases = config["cases"].as_array().unwrap();
    let ids: Vec<&str> = cases.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        [
            "calibration-12",
            "calibration-24",
            "validation-12",
            "validation-24"
        ]
    );
    for case in cases {
        let id = case["id"].as_str().unwrap();
        let population: usize = id.rsplit('-').next().unwrap().parse().unwrap();
        let records: u64 = if id.starts_with("calibration") {
            2048
        } else {
            4096
        };
        let count = std::fs::read_dir(case["creaturesDir"].as_str().unwrap())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x == "json")
            })
            .count();
        assert_eq!(count, population, "{id}");
        let bin = std::fs::metadata(
            std::path::Path::new(case["dataDir"].as_str().unwrap()).join("0.bin"),
        )
        .unwrap();
        assert_eq!(bin.len(), records * (12 + 2) * 4, "{id}");
        assert_distinct_shared_parent(case["creaturesDir"].as_str().unwrap(), population, id);
    }
    let validation = cases.iter().find(|c| c["id"] == "validation-12").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_halving_bench"))
        .arg(validation["creaturesDir"].as_str().unwrap())
        .arg(validation["dataDir"].as_str().unwrap())
        .args(["--cost", "MSE"])
        .env("NEAT_SCORER_ACTIVATION_THREADS", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "ok");
    assert_eq!(v["records"], 4096);
    let arms = v["arms"].as_array().unwrap();
    assert_eq!(arms.len(), 4);
    for arm in arms {
        assert!(arm["maxRelativeDeviation"].as_f64().unwrap() <= 1e-6);
    }
    let errors = v["authoritativeErrors"].as_array().unwrap();
    assert_eq!(errors.len(), 12);
    assert!(
        errors.iter().any(|e| e != &errors[0]),
        "near-clones must differ"
    );

    // Without --generate-fixture the profile option is rejected.
    let bare = Command::new(env!("CARGO_BIN_EXE_halving_bench"))
        .args(["--fixture-profile", "near-clone"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!bare.status.success());
    let bare_err = String::from_utf8_lossy(&bare.stderr);
    assert!(
        bare_err.contains("required") && bare_err.contains("--generate-fixture"),
        "{bare_err}"
    );

    // An existing root is refused and left untouched.
    let existing = dir.path().join("existing");
    std::fs::create_dir(&existing).unwrap();
    std::fs::write(existing.join("marker"), "keep").unwrap();
    let refused = Command::new(env!("CARGO_BIN_EXE_halving_bench"))
        .arg("--generate-fixture")
        .arg(&existing)
        .args(["--fixture-profile", "near-clone"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let refused_text = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        refused_text.contains("create synthetic root"),
        "{refused_text}"
    );
    assert_eq!(
        std::fs::read_to_string(existing.join("marker")).unwrap(),
        "keep"
    );
}

fn assert_distinct_shared_parent(dir: &str, population: usize, id: &str) {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    let texts: Vec<String> = paths
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect();
    let unique: std::collections::HashSet<&String> = texts.iter().collect();
    assert_eq!(unique.len(), texts.len(), "{id}: duplicate candidates");
    let counts: Vec<usize> = texts
        .iter()
        .map(|t| {
            let v: serde_json::Value = serde_json::from_str(t).unwrap();
            v["neurons"].as_array().unwrap().len()
        })
        .collect();
    let n = counts[0];
    let half = population / 2;
    assert!(counts[..half].iter().all(|&c| c == n), "{id}: {counts:?}");
    assert!(
        counts[half..].iter().all(|&c| c + 1 == n),
        "{id}: {counts:?}"
    );
}

#[test]
fn driver_passes_near_clone_profile_through_and_refuses_it_without_synthetic_root() {
    let dir = tempfile::tempdir().unwrap();
    let driver =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/bench-halving.py");
    let python = || {
        let mut c = Command::new("python3");
        c.arg("-B").arg(&driver).stdin(std::process::Stdio::null());
        c
    };
    let manifest = dir.path().join("m.json");
    let out = python()
        .arg("prepare")
        .arg("--synthetic-root")
        .arg(dir.path().join("nc"))
        .args(["--synthetic-profile", "near-clone"])
        .arg("--binary")
        .arg(env!("CARGO_BIN_EXE_halving_bench"))
        .arg("--manifest")
        .arg(&manifest)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let m: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    assert!(
        m["config"]["splitJustification"]
            .as_str()
            .unwrap()
            .contains("one-mutation offspring")
    );
    for case in m["config"]["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let records: u64 = if id.starts_with("calibration") {
            2048
        } else {
            4096
        };
        let bin = std::fs::metadata(
            std::path::Path::new(case["dataDir"].as_str().unwrap()).join("0.bin"),
        )
        .unwrap();
        assert_eq!(bin.len(), records * (12 + 2) * 4, "{id}");
    }

    let any = dir.path().join("any.json");
    std::fs::write(&any, "{}").unwrap();
    let bad_manifest = dir.path().join("x.json");
    let refused = python()
        .arg("prepare")
        .arg("--config")
        .arg(&any)
        .args(["--synthetic-profile", "near-clone"])
        .arg("--manifest")
        .arg(&bad_manifest)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("--synthetic-profile only applies with --synthetic-root")
    );
    assert!(!bad_manifest.exists());
}
