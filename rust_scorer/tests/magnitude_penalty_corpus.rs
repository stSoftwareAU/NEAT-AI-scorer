//! stSoftwareAU/NEAT-AI#3881 — the weight/bias magnitude curve is a
//! cross-engine contract, pinned by a shared corpus rather than by two
//! independently-written sets of expectations.
//!
//! The old `1 / (1 + 1/v)` curve was 0.990 at `|w| = 100` and 0.9999 at
//! `|w| = 1000`, so past about two decades it could no longer tell a sensible
//! weight from an absurd one and production weights reached `1.156e+195`. The
//! replacement charges a constant amount per decade — but only if **both**
//! engines charge the same amount, or `NEAT_AI_RUST_SCORER_STRICT` fires on a
//! creature that scored differently depending on which engine ran it.
//!
//! `tests/fixtures/magnitude-penalty-corpus.json` is a vendored copy of
//! NEAT-AI's `test/fixtures/scoring/magnitude-penalty-corpus.json`, byte for
//! byte. The TypeScript gate is `test/score/MagnitudeSelectionPressure.ts`.

use std::path::{Path, PathBuf};

use rust_scorer::scoring::{MAGNITUDE_DECADE_CAP, magnitude_penalty};
use serde_json::Value;

/// Resolve `tests/fixtures/<name>` relative to this crate.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn corpus() -> Value {
    let path = fixture("magnitude-penalty-corpus.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn cases(corpus: &Value) -> Vec<(f64, f64)> {
    corpus["cases"]
        .as_array()
        .expect("corpus.cases must be an array")
        .iter()
        .map(|c| {
            (
                c["magnitude"].as_f64().expect("magnitude must be a number"),
                c["penalty"].as_f64().expect("penalty must be a number"),
            )
        })
        .collect()
}

#[test]
fn corpus_pins_the_curve() {
    let corpus = corpus();
    let tolerance = corpus["tolerance"].as_f64().expect("tolerance");
    let cases = cases(&corpus);
    assert!(!cases.is_empty(), "corpus must not be empty");

    for (magnitude, expected) in cases {
        let actual = magnitude_penalty(magnitude)
            .unwrap_or_else(|e| panic!("magnitude {magnitude} must score, got {e}"));
        assert!(
            (actual - expected).abs() <= tolerance,
            "magnitude {magnitude} must score {expected}, got {actual}"
        );
    }
}

#[test]
fn corpus_constants_match_this_engine() {
    let corpus = corpus();
    assert_eq!(
        corpus["decadeCap"].as_f64().expect("decadeCap"),
        MAGNITUDE_DECADE_CAP,
        "the corpus and the engine must agree on the decade cap"
    );
    // The clamp bound is stated by the corpus and applied by `magnitude_penalty`.
    let bound = corpus["maxSafeMagnitude"]
        .as_f64()
        .expect("maxSafeMagnitude");
    assert_eq!(
        magnitude_penalty(bound).unwrap(),
        magnitude_penalty(1.1559466326634707e195).unwrap(),
        "anything past the bound must be charged the bound's penalty"
    );
}

#[test]
fn corpus_spans_the_observed_range_and_stays_below_one() {
    let corpus = corpus();
    let cases = cases(&corpus);

    assert!(
        cases.iter().any(|(m, _)| *m <= 1.0) && cases.iter().any(|(m, _)| *m >= 1e20),
        "the corpus must cover magnitudes 1 -> 1e20"
    );
    for (magnitude, penalty) in &cases {
        assert!(
            (0.0..1.0).contains(penalty),
            "penalty for {magnitude} must be in [0, 1), got {penalty}"
        );
    }

    // Monotonic: a bigger magnitude is never cheaper than a smaller one.
    let mut sorted = cases;
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("finite magnitudes"));
    for pair in sorted.windows(2) {
        assert!(
            pair[1].1 >= pair[0].1,
            "penalty must not fall from {} to {}",
            pair[0].0,
            pair[1].0
        );
    }
}

#[test]
fn every_decade_above_one_costs_the_same() {
    let per_decade = magnitude_penalty(10.0).unwrap() - magnitude_penalty(1.0).unwrap();
    assert!(per_decade > 0.0, "a decade of growth must never be free");

    for decade in 1..(MAGNITUDE_DECADE_CAP as i32) {
        let step = magnitude_penalty(10f64.powi(decade + 1)).unwrap()
            - magnitude_penalty(10f64.powi(decade)).unwrap();
        assert!(
            (step - per_decade).abs() < 1e-12,
            "decade {decade} -> {} cost {step}, not {per_decade}",
            decade + 1
        );
    }
}

#[test]
fn sign_is_ignored_and_nan_is_rejected() {
    assert_eq!(
        magnitude_penalty(-4544.0).unwrap(),
        magnitude_penalty(4544.0).unwrap()
    );
    // `f64::min` swallows NaN, so the guard is explicit rather than incidental.
    assert!(magnitude_penalty(f64::NAN).is_err());
}
