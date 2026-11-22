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

fn json(args: &[&str]) -> Value {
    let out = scorer(args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
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
    let plain = json(&["--gpu", "off", &pop, &data]);
    let perturbed = json(&[
        "--gpu",
        "off",
        "--perturb-sigma",
        "0.01",
        "--perturb-seeds",
        "5",
        &pop,
        &data,
    ]);
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
    assert_eq!(p["seeds"], 5);
    assert_eq!(p["sigma"], 0.01);
    assert_eq!(p["target"], "synapseWeights");
    assert_eq!(p["errors"].as_array().unwrap().len(), 5);
    assert!(p["stdError"].as_f64().unwrap() > 0.0);
    assert!(p["weightRms"].as_f64().unwrap() > 0.0);
    let errs: Vec<f64> = p["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_f64().unwrap())
        .collect();
    let mean = errs.iter().sum::<f64>() / 5.0;
    assert!((mean - p["meanError"].as_f64().unwrap()).abs() < 1e-15);

    // Common random numbers: a twin gets exactly the same perturbations; the
    // same synapses listed in another order get the same noise too, so its
    // errors differ only by the f32 rounding of a reordered weighted sum.
    assert_eq!(perturbed["twin"]["perturbation"]["errors"], p["errors"]);
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
    assert_eq!(
        a["perturbation"],
        run("0.02", "0")["perturbation"],
        "same seeds, same result"
    );
    assert_ne!(
        a["perturbation"]["errors"],
        run("0.02", "100")["perturbation"]["errors"]
    );
    let plain = json(&[f, &data]);
    assert_eq!(
        a["error"], plain["error"],
        "single-creature result unchanged"
    );
    // A vanishing sigma reproduces the point estimate; a larger one spreads it.
    let tiny = run("1e-12", "0");
    let point = plain["error"].as_f64().unwrap();
    assert!((tiny["perturbation"]["meanError"].as_f64().unwrap() - point).abs() < 1e-6 * point);
    let wide = run("0.2", "0");
    assert!(
        wide["perturbation"]["stdError"].as_f64().unwrap()
            > a["perturbation"]["stdError"].as_f64().unwrap()
    );
}

#[test]
fn bad_perturbation_flags_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let (pop, data) = fixture(tmp.path());
    let cases: [(&[&str], &str); 7] = [
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
            "--race-stdio or --cohort",
        ),
        (
            &["--perturb-sigma", "0.01", "--race-stdio", &pop, &data],
            "--race-stdio or --cohort",
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
