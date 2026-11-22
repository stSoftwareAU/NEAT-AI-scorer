//! Issue #589 — `--ensemble mean`.
//!
//! The per-creature map must be the one plain directory mode prints, bit for
//! bit, and the ensemble block must satisfy the Krogh–Vedelsby identity.

use std::fs;
use std::path::Path;
use std::process::Command;

use rust_scorer::cohort_fixture::{
    LayeredShape, corpus, graft_cohort, layered_creature, to_le_bytes,
};
use rust_scorer::cost::CostKind;
use rust_scorer::ensemble::score_from_creature_dir_ensemble;
use rust_scorer::fixture_json::{creature_envelope, neuron_json, synapse_json};
use rust_scorer::gpu::GpuBackendLabel;
use rust_scorer::multi_score::score_from_creature_dir_sampled;
use rust_scorer::sampling::SampleSpec;

fn write(dir: &Path, creatures: &[String]) {
    fs::create_dir_all(dir).unwrap();
    for (i, c) in creatures.iter().enumerate() {
        fs::write(dir.join(format!("m{i:02}.json")), c).unwrap();
    }
}

fn write_corpus(dir: &Path, inputs: usize, outputs: usize, files: &[usize]) {
    fs::create_dir_all(dir).unwrap();
    for (f, &n) in files.iter().enumerate() {
        fs::write(
            dir.join(format!("p{f}.bin")),
            to_le_bytes(&corpus(inputs, outputs, n, 40 + f as u64)),
        )
        .unwrap();
    }
}

#[test]
fn per_creature_results_are_directory_mode_bit_for_bit() {
    let shape = LayeredShape::small(5);
    let base = layered_creature(&shape);
    let mut creatures: Vec<String> = graft_cohort(&base, &shape, 3, 1)
        .iter()
        .map(|c| c.to_json())
        .collect();
    creatures.extend((0..3).map(|s| layered_creature(&LayeredShape::small(50 + s)).to_json()));
    for files in [&[1_usize][..], &[3], &[6], &[13, 7], &[517, 5, 1000]] {
        for n in [1, creatures.len()] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("pop");
            let data = tmp.path().join("data");
            write(&dir, &creatures[..n]);
            write_corpus(&data, shape.inputs, shape.outputs, files);
            let sample = SampleSpec::full();
            let (got, report) =
                score_from_creature_dir_ensemble(&dir, &data, CostKind::Mse, &sample, None)
                    .unwrap();
            let want = score_from_creature_dir_sampled(
                &dir,
                &data,
                GpuBackendLabel::CpuFallback,
                CostKind::Mse,
                &sample,
            )
            .unwrap();
            for (k, w) in &want {
                assert_eq!(
                    got[k].error.to_bits(),
                    w.error.to_bits(),
                    "{k} files={files:?} n={n}"
                );
                assert_eq!(got[k].score.to_bits(), w.score.to_bits());
            }
            assert_eq!(report.members, n);
            assert_eq!(
                report.ambiguity,
                report.mean_member_error - report.ensemble_error
            );
            assert!(
                report.ensemble_error <= report.mean_member_error + 1e-12,
                "{report:?}"
            );
            let best = want.values().map(|r| r.error).fold(f64::INFINITY, f64::min);
            assert!(
                (report.best_member_error - best).abs() <= 1e-6 * best,
                "{report:?}"
            );
            if n == 1 {
                assert!(report.ambiguity.abs() < 1e-15, "{report:?}");
            }
        }
    }
}

/// A one-input creature whose single output emits the constant `bias`.
fn constant_creature(bias: f64) -> String {
    creature_envelope(
        1,
        1,
        &[neuron_json("output", "output-0", bias, "IDENTITY")],
        &[synapse_json("input-0", "output-0", 0.0)],
    )
}

fn scorer(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn cli_reports_the_hand_computable_decomposition() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("pop");
    let data = tmp.path().join("data");
    // Members predict +1, -1 and +3 for a target that is always 0.
    write(
        &dir,
        &[
            constant_creature(1.0),
            constant_creature(-1.0),
            constant_creature(3.0),
        ],
    );
    fs::create_dir_all(&data).unwrap();
    let records: Vec<f32> = (0..50).flat_map(|r| [r as f32 * 0.01, 0.0]).collect();
    fs::write(data.join("a.bin"), to_le_bytes(&records)).unwrap();
    let (d, s) = (dir.to_str().unwrap(), data.to_str().unwrap());

    let out = scorer(&["--ensemble", "mean", "--gpu", "off", d, s]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let e = &v["ensemble"];
    // Ē = (1 + 1 + 9) / 3, ensemble predicts 1 → E = 1.
    assert_eq!(e["members"], 3);
    assert!((e["meanMemberError"].as_f64().unwrap() - 11.0 / 3.0).abs() < 1e-12);
    assert!((e["ensembleError"].as_f64().unwrap() - 1.0).abs() < 1e-12);
    assert!((e["ambiguity"].as_f64().unwrap() - 8.0 / 3.0).abs() < 1e-12);
    assert_eq!(e["bestMemberError"].as_f64().unwrap(), 1.0);
    assert_eq!(e["beatsBestMember"], false);
    assert_eq!(
        v["m02"]["error"].as_f64().unwrap(),
        9.0,
        "per-creature entries stay"
    );
    assert!(e.get("score").is_none(), "no score for an ensemble");

    // Top-2 = the ±1 pair: perfect ensemble.
    let out = scorer(&[
        "--ensemble",
        "mean",
        "--ensemble-top-k",
        "2",
        "--gpu",
        "off",
        d,
        s,
    ]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["ensemble"]["memberKeys"],
        serde_json::json!(["m00", "m01"])
    );
    assert_eq!(v["ensemble"]["ensembleError"].as_f64().unwrap(), 0.0);
    assert_eq!(v["ensemble"]["beatsBestMember"], true);

    for bad in [
        &["--ensemble", "mean", "--ensemble-top-k", "4", d, s][..],
        &["--ensemble", "mean", "--ensemble-top-k", "0", d, s],
        &["--ensemble", "mean", "--cost", "MAE", d, s],
        &["--ensemble", "mean", "--cohort", d, s],
        &["--ensemble-top-k", "2", d, s],
    ] {
        assert!(!scorer(bad).status.success(), "{bad:?} must fail");
    }
}

#[test]
fn recurrent_members_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("pop");
    let data = tmp.path().join("data");
    let recurrent = constant_creature(1.0).replace("\"forwardOnly\":true", "\"forwardOnly\":false");
    write(&dir, &[constant_creature(1.0), recurrent]);
    write_corpus(&data, 1, 1, &[10]);
    let err =
        score_from_creature_dir_ensemble(&dir, &data, CostKind::Mse, &SampleSpec::full(), None)
            .unwrap_err();
    assert!(err.contains("forward-only"), "{err}");
}
