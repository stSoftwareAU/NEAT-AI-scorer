//! Issue #683 — cohort (delta) scoring parity.
//!
//! The gate: for every creature, the cohort result equals its standalone
//! result. "Standalone" is the creature scored alone in a directory, which
//! partitions records across workers the way the cohort sweep does, so the
//! comparison is **bit for bit**. Against a multi-creature directory run —
//! where directory mode itself regroups records per worker (Issue #537) — the
//! documented tolerance is `1e-6` relative; that drift is directory mode's own,
//! and the test checks directory mode shows it against standalone too.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use rust_scorer::cohort::{
    CohortMember, CohortPlan, CohortRejection, CohortRun, score_from_creature_dir_cohort,
};
use rust_scorer::cohort_fixture::{
    CreatureSpec, LayeredShape, corpus, graft_cohort, layered_creature, removal_cohort,
    to_le_bytes, weight_change_cohort,
};
use rust_scorer::cost::CostKind;
use rust_scorer::gpu::GpuBackendLabel;
use rust_scorer::multi_score::score_from_creature_dir_sampled;
use rust_scorer::sampling::SampleSpec;
use rust_scorer::scoring::ScoreResult;

const REGROUP_TOLERANCE: f64 = 1e-6;

fn write_dir(dir: &Path, cohort: &[CreatureSpec]) {
    fs::create_dir_all(dir).unwrap();
    for (i, c) in cohort.iter().enumerate() {
        fs::write(dir.join(format!("c{i:03}.json")), c.to_json()).unwrap();
    }
}

/// Write the corpus as several `.bin` files of the given record counts.
fn write_corpus(dir: &Path, inputs: usize, outputs: usize, files: &[usize], seed: u64) {
    fs::create_dir_all(dir).unwrap();
    for (f, &n) in files.iter().enumerate() {
        let recs = corpus(inputs, outputs, n, seed + f as u64);
        fs::write(dir.join(format!("part-{f:02}.bin")), to_le_bytes(&recs)).unwrap();
    }
}

fn directory(dir: &Path, data: &Path, sample: &SampleSpec) -> BTreeMap<String, ScoreResult> {
    score_from_creature_dir_sampled(
        dir,
        data,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
        sample,
    )
    .expect("directory scoring")
}

fn cohort(
    dir: &Path,
    data: &Path,
    cost: CostKind,
    sample: &SampleSpec,
) -> BTreeMap<String, ScoreResult> {
    match score_from_creature_dir_cohort(dir, data, cost, sample).expect("cohort scoring") {
        CohortRun::Hosted { results, .. } => results,
        CohortRun::Unhostable(r) => panic!("cohort unexpectedly unhostable: {r}"),
    }
}

/// Score each creature alone in its own one-creature directory.
fn standalone(
    root: &Path,
    cohort: &[CreatureSpec],
    data: &Path,
    sample: &SampleSpec,
) -> BTreeMap<String, ScoreResult> {
    let mut out = BTreeMap::new();
    for (i, c) in cohort.iter().enumerate() {
        let key = format!("c{i:03}");
        let dir = root.join(format!("alone-{key}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{key}.json")), c.to_json()).unwrap();
        let mut one = directory(&dir, data, sample);
        out.insert(key.clone(), one.remove(&key).unwrap());
    }
    out
}

fn ranking(results: &BTreeMap<String, ScoreResult>) -> Vec<String> {
    let mut keys: Vec<&String> = results.keys().collect();
    keys.sort_by(|a, b| {
        results[*a]
            .error
            .total_cmp(&results[*b].error)
            .then(a.cmp(b))
    });
    keys.into_iter().cloned().collect()
}

fn assert_parity(
    name: &str,
    members: &[CreatureSpec],
    shape: &LayeredShape,
    files: &[usize],
    sample: SampleSpec,
) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("cohort");
    let data = tmp.path().join("data");
    write_dir(&dir, members);
    write_corpus(&data, shape.inputs, shape.outputs, files, 7);

    let got = cohort(&dir, &data, CostKind::Mse, &sample);
    let alone = standalone(tmp.path(), members, &data, &sample);
    assert_eq!(got.len(), members.len(), "{name}: one result per creature");
    for (key, want) in &alone {
        let r = &got[key];
        assert_eq!(
            r.error.to_bits(),
            want.error.to_bits(),
            "{name} {key} files={files:?}: cohort error {} != standalone {}",
            r.error,
            want.error
        );
        assert_eq!(
            r.score.to_bits(),
            want.score.to_bits(),
            "{name} {key}: score"
        );
        assert_eq!(r.record_count, want.record_count, "{name} {key}: records");
        assert_eq!(r.hidden_neurons, want.hidden_neurons);
        assert_eq!(r.synapse_count, want.synapse_count);
        assert_eq!(
            r.complexity_penalty.to_bits(),
            want.complexity_penalty.to_bits()
        );
    }
    assert_eq!(ranking(&got), ranking(&alone), "{name}: candidate ordering");

    let dirmode = directory(&dir, &data, &sample);
    let rel = |a: f64, b: f64| (a - b).abs() / b.abs().max(1e-300);
    for (key, want) in &dirmode {
        let cohort_drift = rel(got[key].error, want.error);
        let directory_drift = rel(alone[key].error, want.error);
        assert!(
            cohort_drift <= REGROUP_TOLERANCE,
            "{name} {key}: cohort vs directory rel {cohort_drift:e}"
        );
        // The drift is directory mode's regrouping, not the cohort's.
        assert_eq!(
            cohort_drift.to_bits(),
            directory_drift.to_bits(),
            "{name} {key}"
        );
    }
}

/// Record counts chosen to hit every SIMD tier of a worker range: scalar-only
/// ranges (< 4), a lone 4-group, 8-tiles plus tails, and full 32-tiles across
/// several files.
const CORPORA: [&[usize]; 5] = [&[1], &[3], &[4, 3], &[7, 37], &[517, 5, 1000]];

#[test]
fn graft_cohort_is_bit_identical_to_standalone() {
    let shape = LayeredShape::small(11);
    let base = layered_creature(&shape);
    let members = graft_cohort(&base, &shape, 9, 3);
    for files in CORPORA {
        assert_parity("graft", &members, &shape, files, SampleSpec::full());
    }
}

#[test]
fn weight_change_cohort_is_bit_identical_to_standalone() {
    let shape = LayeredShape::small(12);
    let base = layered_creature(&shape);
    let members = weight_change_cohort(&base, 10, 4);
    for files in CORPORA {
        assert_parity("weight", &members, &shape, files, SampleSpec::full());
    }
}

#[test]
fn removal_cohort_is_bit_identical_to_standalone() {
    let shape = LayeredShape::small(13);
    let base = layered_creature(&shape);
    let members = removal_cohort(&base, &shape, 10, 5);
    for files in CORPORA {
        assert_parity("removal", &members, &shape, files, SampleSpec::full());
    }
}

#[test]
fn unrelated_creatures_are_still_scored_exactly() {
    // No sharing at all: every creature is its own union component.
    let members: Vec<CreatureSpec> = (0..5)
        .map(|s| layered_creature(&LayeredShape::small(100 + s)))
        .collect();
    assert_parity(
        "unrelated",
        &members,
        &LayeredShape::small(100),
        &[517, 5, 1000],
        SampleSpec::full(),
    );
}

#[test]
fn sampled_cohort_is_bit_identical_to_sampled_standalone() {
    let shape = LayeredShape::small(14);
    let base = layered_creature(&shape);
    let members = graft_cohort(&base, &shape, 6, 8);
    let sample = SampleSpec::new(0.37, 3).unwrap();
    assert_parity("sampled", &members, &shape, &[517, 5, 1000], sample);
}

#[test]
fn rmse_cohort_matches_standalone() {
    let shape = LayeredShape::small(15);
    let base = layered_creature(&shape);
    let members = weight_change_cohort(&base, 4, 9);
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("cohort");
    let data = tmp.path().join("data");
    write_dir(&dir, &members);
    write_corpus(&data, shape.inputs, shape.outputs, &[300], 1);
    let got = cohort(&dir, &data, CostKind::Rmse, &SampleSpec::full());
    for (i, c) in members.iter().enumerate() {
        let key = format!("c{i:03}");
        let alone = tmp.path().join(format!("alone-{key}"));
        write_dir(&alone, std::slice::from_ref(c));
        let want = score_from_creature_dir_sampled(
            &alone,
            &data,
            GpuBackendLabel::CpuFallback,
            CostKind::Rmse,
            &SampleSpec::full(),
        )
        .unwrap();
        assert_eq!(got[&key].error.to_bits(), want["c000"].error.to_bits());
        assert_eq!(got[&key].cost_name, "RMSE");
    }
}

fn plan_for(members: &[CreatureSpec], cost: CostKind) -> Result<CohortPlan, CohortRejection> {
    let exports: Vec<_> = members
        .iter()
        .map(|c| neat_core::creature::parse_creature_json(&c.to_json()).unwrap())
        .collect();
    let nets: Vec<_> = exports
        .iter()
        .map(|e| neat_core::creature::compile_creature(e).unwrap())
        .collect();
    let keys: Vec<String> = (0..members.len()).map(|i| format!("c{i}")).collect();
    let m: Vec<CohortMember<'_>> = nets
        .iter()
        .zip(&exports)
        .zip(&keys)
        .map(|((network, e), key)| CohortMember {
            key,
            network,
            forward_only: e.forward_only,
        })
        .collect();
    CohortPlan::build(&m, members[0].inputs, members[0].outputs, cost)
}

#[test]
fn graft_union_pays_only_for_the_dirty_cone() {
    let shape = LayeredShape::small(16);
    let base = layered_creature(&shape);
    let members = graft_cohort(&base, &shape, 9, 3);
    let stats = plan_for(&members, CostKind::Mse).unwrap().stats();
    // The incumbent alone: its three identical `1.0` constants are one
    // computation, so its union is already two nodes smaller than it.
    let incumbent = plan_for(&members[..1], CostKind::Mse)
        .unwrap()
        .stats()
        .union_neurons;
    assert_eq!(incumbent, base.neuron_count() - 2);
    let base_neurons = base.neuron_count();
    let grafted: usize = members
        .iter()
        .skip(1)
        .map(|c| c.neuron_count() - base_neurons)
        .sum();
    // Each graft re-pays only its IF neurons, the anchor and the outputs the
    // anchor feeds; everything else is the incumbent's, activated once.
    let dirty_per_graft = 1 + shape.outputs;
    assert_eq!(stats.creatures, 9);
    assert_eq!(
        stats.union_neurons,
        incumbent + grafted + 8 * dirty_per_graft
    );
    assert_eq!(
        stats.standalone_neurons,
        members
            .iter()
            .map(CreatureSpec::neuron_count)
            .sum::<usize>()
    );
    assert!(stats.union_neurons * 4 < stats.standalone_neurons);
}

#[test]
fn identical_creatures_share_everything() {
    let base = layered_creature(&LayeredShape::small(17));
    let one = plan_for(std::slice::from_ref(&base), CostKind::Mse)
        .unwrap()
        .stats()
        .union_neurons;
    let stats = plan_for(&[base.clone(), base.clone(), base.clone()], CostKind::Mse)
        .unwrap()
        .stats();
    assert_eq!(stats.union_neurons, one);
    assert_eq!(stats.standalone_neurons, 3 * base.neuron_count());
}

#[test]
fn negative_zero_weight_is_a_different_computation() {
    let base = layered_creature(&LayeredShape::small(18));
    let mut a = base.clone();
    let mut b = base.clone();
    let idx = a.synapses.len() - 1;
    a.synapses[idx].weight = 0.0;
    b.synapses[idx].weight = -0.0;
    let one = plan_for(std::slice::from_ref(&a), CostKind::Mse)
        .unwrap()
        .stats()
        .union_neurons;
    let stats = plan_for(&[a, b], CostKind::Mse).unwrap().stats();
    assert_eq!(stats.union_neurons, one + 1, "only the output differs");
}

#[test]
fn unhostable_cohorts_are_refused_before_scoring() {
    let shape = LayeredShape::small(19);
    let base = layered_creature(&shape);
    assert!(matches!(
        plan_for(std::slice::from_ref(&base), CostKind::Mae),
        Err(CohortRejection::UnsupportedCost("MAE"))
    ));

    let mut hypot = base.clone();
    hypot.neurons[5].squash = "HYPOT".to_string();
    assert!(matches!(
        plan_for(&[base.clone(), hypot], CostKind::Mse),
        Err(CohortRejection::UnsupportedSquash { .. })
    ));

    let recurrent = base
        .to_json()
        .replace("\"forwardOnly\":true", "\"forwardOnly\":false");
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("cohort");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a.json"), base.to_json()).unwrap();
    fs::write(dir.join("b.json"), recurrent).unwrap();
    // The data directory does not exist: a refusal must come before any read.
    let missing = tmp.path().join("no-data");
    match score_from_creature_dir_cohort(&dir, &missing, CostKind::Mse, &SampleSpec::full())
        .unwrap()
    {
        CohortRun::Unhostable(CohortRejection::Recurrent(key)) => assert_eq!(key, "b"),
        _ => panic!("recurrent cohort must be refused"),
    }
}

fn scorer(args: &[&str]) -> (serde_json::Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        serde_json::from_slice(&out.stdout).unwrap(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn cli_cohort_flag_hosts_and_falls_back_loudly() {
    let shape = LayeredShape::small(20);
    let base = layered_creature(&shape);
    let members = graft_cohort(&base, &shape, 5, 2);
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("cohort");
    let data = tmp.path().join("data");
    write_dir(&dir, &members);
    write_corpus(&data, shape.inputs, shape.outputs, &[200, 33], 3);
    let (d, s) = (dir.to_str().unwrap(), data.to_str().unwrap());

    let (hosted, stderr) = scorer(&["--cohort", "--gpu", "off", d, s]);
    assert!(stderr.contains("[cohort] hosted 5 creatures"), "{stderr}");
    let alone = standalone(tmp.path(), &members, &data, &SampleSpec::full());
    for (key, want) in &alone {
        assert_eq!(
            hosted[key]["error"].as_f64().unwrap().to_bits(),
            want.error.to_bits(),
            "{key}"
        );
    }

    let (fallback, stderr) = scorer(&["--cohort", "--gpu", "off", "--cost", "MAE", d, s]);
    assert_eq!(
        stderr
            .matches("[cohort] fallback to directory mode:")
            .count(),
        1,
        "{stderr}"
    );
    let (plain, _) = scorer(&["--gpu", "off", "--cost", "MAE", d, s]);
    for key in alone.keys() {
        assert_eq!(fallback[key]["error"], plain[key]["error"], "{key}");
    }
}

#[test]
fn cli_cohort_rejects_incompatible_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let base = layered_creature(&LayeredShape::small(21));
    let pop = tmp.path().join("pop");
    write_dir(&pop, std::slice::from_ref(&base));
    let file = pop.join("c000.json");
    let data = tmp.path().join("data");
    write_corpus(&data, 12, 2, &[10], 1);
    let (p, f, d) = (
        pop.to_str().unwrap(),
        file.to_str().unwrap(),
        data.to_str().unwrap(),
    );
    let refused = |args: &[&str], message: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?} must be refused");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(message),
            "{args:?}: unexpected stderr: {stderr}"
        );
    };
    refused(
        &["--cohort", f, d],
        "--cohort requires a creatures directory",
    );
    // Named before adapter selection, so a GPU-less host reports the conflict.
    refused(
        &["--cohort", "--gpu", "on", p, d],
        "--cohort runs on the CPU",
    );
    refused(
        &["--cohort", "--race-stdio", p, d],
        "--cohort cannot be combined with --race-stdio",
    );
    refused(
        &["--cohort", "--creature-stdin", d],
        "cannot be combined with --creature-stdin",
    );
}
