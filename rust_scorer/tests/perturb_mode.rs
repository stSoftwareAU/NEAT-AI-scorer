//! Issue #588 — perturbation-robustness mode.

use std::fs;
use std::path::Path;
use std::process::Command;

use rust_scorer::cohort_fixture::{
    LayeredShape, corpus, layered_creature, to_le_bytes, weight_change_cohort,
};
use serde_json::Value;

fn scorer(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .args(args)
        .output()
        .unwrap()
}

/// Run with a pinned activation-thread count. 32 threads over 25 perturbed
/// copies (5 creatures x 5 seeds) would give a ragged directory split.
fn json_with_threads(threads: &str, args: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .env("NEAT_SCORER_ACTIVATION_THREADS", threads)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn floats(v: &Value) -> Vec<f64> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_f64().unwrap())
        .collect()
}

fn mean_and_sample_std(v: &[f64]) -> (f64, f64) {
    let n = v.len() as f64;
    let mean = v.iter().sum::<f64>() / n;
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
    (mean, var.sqrt())
}

/// Every summary field is consistent with its per-seed arrays, and the score
/// side mirrors the error side the way the point result does.
fn assert_summary_consistent(entry: &Value, seeds: usize, seed_base: u64) {
    let p = &entry["perturbation"];
    let errors = floats(&p["errors"]);
    let scores = floats(&p["scores"]);
    assert_eq!(errors.len(), seeds);
    assert_eq!(scores.len(), seeds);
    assert_eq!(p["seeds"], seeds);
    assert_eq!(p["seedBase"], seed_base);
    let close = |a: f64, b: f64| (a - b).abs() <= 1e-12 * b.abs().max(1.0);
    let (me, se) = mean_and_sample_std(&errors);
    let (ms, ss) = mean_and_sample_std(&scores);
    assert!(close(p["meanError"].as_f64().unwrap(), me), "{p}");
    assert!(close(p["stdError"].as_f64().unwrap(), se), "{p}");
    assert!(close(p["meanScore"].as_f64().unwrap(), ms), "{p}");
    assert!(close(p["stdScore"].as_f64().unwrap(), ss), "{p}");
    // score = 1 - error - penalties, and the penalties barely move under a
    // small perturbation: score + error tracks the point result's.
    let point = entry["score"].as_f64().unwrap() + entry["error"].as_f64().unwrap();
    for (s, e) in scores.iter().zip(&errors) {
        assert!(
            (s + e - point).abs() < 1e-4,
            "score {s} / error {e} vs point {point}"
        );
    }
}

fn json(args: &[&str]) -> Value {
    let out = scorer(args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn fixture_c0() -> rust_scorer::cohort_fixture::CreatureSpec {
    let base = layered_creature(&LayeredShape::small(31));
    weight_change_cohort(&base, 3, 2).remove(0)
}

fn fixture(dir: &Path) -> (String, String) {
    let shape = LayeredShape::small(31);
    let base = layered_creature(&shape);
    let pop = dir.join("pop");
    fs::create_dir_all(&pop).unwrap();
    for (i, c) in weight_change_cohort(&base, 3, 2).iter().enumerate() {
        fs::write(pop.join(format!("c{i}.json")), c.to_json()).unwrap();
    }
    // An exact twin of c0, and c0 with its synapses listed in reverse order.
    let mut twin = weight_change_cohort(&base, 3, 2).remove(0);
    fs::write(pop.join("twin.json"), twin.to_json()).unwrap();
    twin.synapses.reverse();
    fs::write(pop.join("reordered.json"), twin.to_json()).unwrap();
    let data = dir.join("data");
    fs::create_dir_all(&data).unwrap();
    fs::write(
        data.join("a.bin"),
        to_le_bytes(&corpus(shape.inputs, shape.outputs, 301, 9)),
    )
    .unwrap();
    (
        pop.to_str().unwrap().to_string(),
        data.to_str().unwrap().to_string(),
    )
}

fn without_perturbation(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(map) = v.as_object_mut() {
        for entry in map.values_mut() {
            if let Some(e) = entry.as_object_mut() {
                e.remove("perturbation");
                e.remove("timeTaken");
                e.remove("compileTimeSecs");
            }
        }
        map.remove("perturbation");
        map.remove("timeTaken");
        map.remove("compileTimeSecs");
    }
    v
}

#[test]
fn directory_results_are_unchanged_and_carry_paired_summaries() {
    let tmp = tempfile::tempdir().unwrap();
    let (pop, data) = fixture(tmp.path());
    let plain = json_with_threads("32", &["--gpu", "off", &pop, &data]);
    let perturbed = json_with_threads(
        "32",
        &[
            "--gpu",
            "off",
            "--perturb-sigma",
            "0.01",
            "--perturb-seeds",
            "5",
            &pop,
            &data,
        ],
    );
    assert_eq!(
        without_perturbation(&plain),
        without_perturbation(&perturbed),
        "unperturbed results unchanged"
    );
    assert!(
        plain["c0"].get("perturbation").is_none(),
        "plain run JSON unchanged"
    );

    let p = &perturbed["c0"]["perturbation"];
    assert_eq!(p["sigma"], 0.01);
    assert_eq!(p["target"], "synapseWeights");
    assert!(p["stdError"].as_f64().unwrap() > 0.0);
    assert!(p["stdScore"].as_f64().unwrap() > 0.0);
    for key in ["c0", "c1", "c2", "twin", "reordered"] {
        assert_summary_consistent(&perturbed[key], 5, 0);
    }
    // The calibration scale is the unperturbed weights' own.
    let c0 = fixture_c0();
    let mut abs: Vec<f64> = c0.synapses.iter().map(|s| s.weight.abs()).collect();
    abs.sort_by(f64::total_cmp);
    let n = abs.len();
    let median = if n % 2 == 1 {
        abs[n / 2]
    } else {
        (abs[n / 2 - 1] + abs[n / 2]) / 2.0
    };
    let rms = (c0.synapses.iter().map(|s| s.weight * s.weight).sum::<f64>() / n as f64).sqrt();
    assert_eq!(p["weightMedianAbs"].as_f64().unwrap(), median);
    assert!((p["weightRms"].as_f64().unwrap() - rms).abs() < 1e-15);
    let errs = floats(&p["errors"]);

    // Common random numbers: a twin gets exactly the same perturbations; the
    // same synapses listed in another order get the same noise too, so its
    // errors differ only by the f32 rounding of a reordered weighted sum.
    // Bit-identical under a thread count (32) that would split 25 copies
    // raggedly in directory mode: the perturbation sweep's partition is uniform.
    assert_eq!(perturbed["twin"]["perturbation"]["errors"], p["errors"]);
    assert_eq!(perturbed["twin"]["perturbation"]["scores"], p["scores"]);
    let reordered = perturbed["reordered"]["perturbation"]["errors"]
        .as_array()
        .unwrap();
    for (r, e) in reordered.iter().zip(&errs) {
        let r = r.as_f64().unwrap();
        assert!((r - e).abs() <= 1e-6 * e, "reordered {r} vs {e}");
    }
    // Different creatures still differ.
    assert_ne!(perturbed["c1"]["perturbation"]["errors"], p["errors"]);
}

#[test]
fn perturbations_are_deterministic_and_scale_with_sigma() {
    let tmp = tempfile::tempdir().unwrap();
    let (pop, data) = fixture(tmp.path());
    let file = Path::new(&pop).join("c0.json");
    let f = file.to_str().unwrap();
    let run = |sigma: &str, base: &str| {
        json(&[
            "--perturb-sigma",
            sigma,
            "--perturb-seeds",
            "6",
            "--perturb-seed-base",
            base,
            f,
            &data,
        ])
    };
    let a = run("0.02", "0");
    assert_summary_consistent(&a, 6, 0);
    assert_eq!(
        a["perturbation"],
        run("0.02", "0")["perturbation"],
        "same seeds, same result"
    );
    let shifted = run("0.02", "100");
    assert_summary_consistent(&shifted, 6, 100);
    assert_ne!(
        a["perturbation"]["errors"],
        shifted["perturbation"]["errors"]
    );
    assert_ne!(
        a["perturbation"]["scores"],
        shifted["perturbation"]["scores"]
    );
    let plain = json(&[f, &data]);
    assert_eq!(
        a["error"], plain["error"],
        "single-creature error unchanged"
    );
    assert_eq!(
        a["score"], plain["score"],
        "single-creature score unchanged"
    );
    // A vanishing sigma reproduces the point estimate on both sides; a larger
    // one spreads it.
    let tiny = run("1e-12", "0");
    let point_error = plain["error"].as_f64().unwrap();
    let point_score = plain["score"].as_f64().unwrap();
    assert!(
        (tiny["perturbation"]["meanError"].as_f64().unwrap() - point_error).abs()
            < 1e-6 * point_error
    );
    assert!(
        (tiny["perturbation"]["meanScore"].as_f64().unwrap() - point_score).abs()
            < 1e-6 * point_score.abs()
    );
    let wide = run("0.2", "0");
    for key in ["stdError", "stdScore"] {
        assert!(
            wide["perturbation"][key].as_f64().unwrap() > a["perturbation"][key].as_f64().unwrap(),
            "{key}"
        );
    }
}

#[test]
fn bad_perturbation_flags_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let (pop, data) = fixture(tmp.path());
    let cases: [(&[&str], &str); 8] = [
        (
            &["--perturb-sigma", "0", &pop, &data],
            "--perturb-sigma must be a finite value > 0",
        ),
        (
            &["--perturb-sigma=-1", &pop, &data],
            "--perturb-sigma must be a finite value > 0",
        ),
        (
            &[
                "--perturb-sigma",
                "0.01",
                "--perturb-seeds",
                "0",
                &pop,
                &data,
            ],
            "--perturb-seeds must be in 1..=",
        ),
        (&["--perturb-seeds", "4", &pop, &data], "--perturb-sigma"),
        (
            &["--perturb-sigma", "0.01", "--cohort", &pop, &data],
            "--race-stdio, --cohort or --ensemble",
        ),
        (
            &["--perturb-sigma", "0.01", "--race-stdio", &pop, &data],
            "--race-stdio, --cohort or --ensemble",
        ),
        (
            &["--perturb-sigma", "0.01", "--ensemble", "mean", &pop, &data],
            "--race-stdio, --cohort or --ensemble",
        ),
        // Named before adapter selection, so a GPU-less host reports the conflict.
        (
            &["--perturb-sigma", "0.01", "--gpu", "on", &pop, &data],
            "scores its copies on the CPU",
        ),
    ];
    for (args, message) in cases {
        let out = scorer(args);
        assert!(!out.status.success(), "{args:?} must fail");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(message),
            "{args:?}: unexpected stderr: {stderr}"
        );
    }
}
