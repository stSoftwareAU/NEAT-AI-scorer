//! Issue #683 — cohort parity under a many-worker partition, on the Issue #574
//! `IF` decision-tree parity fixtures.
//!
//! One test per file: it pins `NEAT_SCORER_ACTIVATION_THREADS` for the whole
//! process so every chunk is split across several workers (the partition the
//! cohort sweep mirrors), which a parallel test in the same binary must not
//! observe.

use std::fs;

use rust_scorer::cohort::{CohortRun, score_from_creature_dir_cohort};
use rust_scorer::cost::CostKind;
use rust_scorer::gpu::GpuBackendLabel;
use rust_scorer::if_tree_fixture::{
    TreeSpec, boundary_records, corpus_records, grafted_creature_json,
    mixed_neural_if_creature_json, records_to_le_bytes, tree_creature_json,
    ungrafted_creature_json,
};
use rust_scorer::multi_score::score_from_creature_dir_sampled;
use rust_scorer::sampling::SampleSpec;

#[test]
fn if_tree_cohort_is_bit_identical_to_standalone_across_workers() {
    // SAFETY: the only test in this binary, set before any scoring thread runs.
    unsafe { std::env::set_var("NEAT_SCORER_ACTIVATION_THREADS", "5") };
    let inputs = 9;
    let oracle = TreeSpec::new(inputs, 3, 1);
    let mut creatures = vec![
        ungrafted_creature_json(&oracle, 24),
        grafted_creature_json(&oracle, 24),
        grafted_creature_json(&TreeSpec::new(inputs, 1, 2), 24),
        mixed_neural_if_creature_json(&oracle, 6),
    ];
    for seed in 0..6 {
        creatures.push(tree_creature_json(&TreeSpec::new(
            inputs,
            1 + (seed % 3) as u32,
            seed,
        )));
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("cohort");
    let data = tmp.path().join("data");
    fs::create_dir_all(&dir).unwrap();
    fs::create_dir_all(&data).unwrap();
    for (i, c) in creatures.iter().enumerate() {
        fs::write(dir.join(format!("c{i:02}.json")), c).unwrap();
    }
    fs::write(
        data.join("a.bin"),
        records_to_le_bytes(&corpus_records(&oracle, 1203)),
    )
    .unwrap();
    fs::write(
        data.join("b.bin"),
        records_to_le_bytes(&boundary_records(&oracle)),
    )
    .unwrap();
    fs::write(
        data.join("c.bin"),
        records_to_le_bytes(&corpus_records(&oracle, 6)),
    )
    .unwrap();

    let CohortRun::Hosted { results, stats } =
        score_from_creature_dir_cohort(&dir, &data, CostKind::Mse, &SampleSpec::full()).unwrap()
    else {
        panic!("IF tree cohort must be hostable");
    };
    assert!(stats.union_neurons < stats.standalone_neurons);
    for (i, c) in creatures.iter().enumerate() {
        let key = format!("c{i:02}");
        let alone = tmp.path().join(&key);
        fs::create_dir_all(&alone).unwrap();
        fs::write(alone.join(format!("{key}.json")), c).unwrap();
        let want = score_from_creature_dir_sampled(
            &alone,
            &data,
            GpuBackendLabel::CpuFallback,
            CostKind::Mse,
            &SampleSpec::full(),
        )
        .unwrap();
        assert_eq!(results[&key].activation_threads, Some(5));
        assert_eq!(
            results[&key].error.to_bits(),
            want[&key].error.to_bits(),
            "{key}: cohort {} vs standalone {}",
            results[&key].error,
            want[&key].error
        );
    }
}
