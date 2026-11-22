//! Issue #683 — `cohort_bench` smoke test on the small parity shape.

use std::process::Command;

#[test]
fn cohort_bench_reports_every_cohort_with_a_real_interval() {
    let out = Command::new(env!("CARGO_BIN_EXE_cohort_bench"))
        .args(["--shape", "small", "--bytes", "150000", "--repeats", "6"])
        .output()
        .expect("run cohort_bench");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).expect("one JSON object per line"))
        .collect();
    let names: Vec<&str> = rows.iter().map(|r| r["cohort"].as_str().unwrap()).collect();
    assert_eq!(names, ["graft", "weight", "removal"]);
    for r in &rows {
        let act = &r["neuronActivationsPerRecord"];
        assert!(
            act["cohort"].as_u64().unwrap() < act["directory"].as_u64().unwrap(),
            "{r}"
        );
        assert!(r["maxRelErrorVsDirectory"].as_f64().unwrap() <= 1e-6, "{r}");
        for mode in ["directory", "cohort"] {
            let w = &r["wallMs"][mode];
            assert_eq!(w["runs"].as_array().unwrap().len(), 6);
            let ci = &w["ci95"];
            assert!(ci["coverage"].as_f64().unwrap() >= 0.95, "{r}");
            assert!(ci["low"].as_f64().unwrap() <= w["median"].as_f64().unwrap());
            assert!(ci["high"].as_f64().unwrap() >= w["median"].as_f64().unwrap());
        }
    }
    assert_eq!(rows[0]["creatures"], 64);
    assert_eq!(rows[1]["creatures"], 100);
}

#[test]
fn cohort_bench_reports_no_interval_below_six_runs() {
    let out = Command::new(env!("CARGO_BIN_EXE_cohort_bench"))
        .args([
            "--shape",
            "small",
            "--bytes",
            "20000",
            "--repeats",
            "2",
            "--cohorts",
            "weight",
        ])
        .output()
        .expect("run cohort_bench");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let row: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(row["wallMs"]["directory"]["ci95"].is_null());
    assert!(row["wallMs"]["cohort"]["ci95"].is_null());
}

#[test]
fn cohort_bench_refuses_unknown_shapes_and_zero_repeats() {
    let cases: [(&[&str], &str); 2] = [
        (&["--shape", "huge"], "unknown --shape 'huge'"),
        (&["--repeats", "0"], "--repeats must be at least 1"),
    ];
    for (args, message) in cases {
        let out = Command::new(env!("CARGO_BIN_EXE_cohort_bench"))
            .args(args)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(message),
            "{args:?}: unexpected stderr: {stderr}"
        );
        assert!(
            !stderr.contains("panicked"),
            "{args:?} must be a named refusal: {stderr}"
        );
    }
}
