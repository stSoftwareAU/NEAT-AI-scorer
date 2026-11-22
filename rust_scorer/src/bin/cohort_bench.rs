//! Cohort (delta) scoring bench — Issue #683.
//!
//! Scores three near-clone cohorts of a **production-scale** synthetic creature
//! ([`LayeredShape::production_scale`]: 2 511 inputs, 7 804 non-input
//! neurons, 65 687 synapses) both ways on the same corpus and host:
//!
//! * `graft` — the incumbent plus 63 candidates, each grafted with 1–4 `IF`
//!   neurons at one fan-in-1 500 anchor (64 creatures);
//! * `weight` — 100 single-weight changes spread through the network;
//! * `removal` — 100 single-neuron removals.
//!
//! For each it reports neuron activations per record (cohort union versus the
//! sum over creatures that directory mode pays), and wall time of directory
//! mode and cohort mode: the median of `--repeats` alternating runs with a
//! distribution-free 95 % confidence interval of the median
//! ([`median_ci95`], `null` below 6 runs, where none exists). The
//! predeclared bar is the 64-graft cohort at **≤ 25 %** of directory mode's
//! wall time.
//!
//! ```text
//! cargo build --release -p rust_scorer --bin cohort_bench
//! BENCH_SCORING_BYTES=200000000 ./target/release/cohort_bench --repeats 6
//! ```
//!
//! Output is one JSON object per cohort on stdout. Fail-loud: a scoring error
//! or a cohort that falls back exits non-zero.

use std::fs;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;

use rust_scorer::bench_support::{median_ci95, median_ms};
use rust_scorer::cohort::{CohortRun, score_from_creature_dir_cohort};
use rust_scorer::cohort_fixture::{
    CreatureSpec, LayeredShape, corpus, graft_cohort, layered_creature, removal_cohort,
    to_le_bytes, weight_change_cohort,
};
use rust_scorer::cost::CostKind;
use rust_scorer::gpu::GpuBackendLabel;
use rust_scorer::multi_score::score_from_creature_dir_sampled;
use rust_scorer::sampling::SampleSpec;

#[derive(Parser, Debug)]
#[command(name = "cohort_bench")]
struct Cli {
    /// Timed repetitions of each mode (alternating); the median is reported.
    /// At least 6 are needed for a 95 % interval of the median to exist.
    #[arg(long, default_value_t = 6)]
    repeats: usize,

    /// Creature shape: `production` (Issue #683's production scale) or
    /// `small` (the parity-test shape, for a quick smoke run).
    #[arg(long, default_value = "production")]
    shape: String,

    /// Corpus size in bytes; falls back to `BENCH_SCORING_BYTES`, then 200 MB.
    #[arg(long)]
    bytes: Option<usize>,

    /// Comma-separated subset of `graft,weight,removal`.
    #[arg(long, default_value = "graft,weight,removal")]
    cohorts: String,

    /// Fixture seed.
    #[arg(long, default_value_t = 683)]
    seed: u64,
}

fn write_cohort(dir: &Path, members: &[CreatureSpec]) -> std::io::Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    fs::create_dir_all(dir)?;
    for (i, c) in members.iter().enumerate() {
        fs::write(dir.join(format!("c{i:03}.json")), c.to_json())?;
    }
    Ok(())
}

fn run(cli: &Cli) -> Result<(), String> {
    let bytes = cli
        .bytes
        .or_else(|| std::env::var("BENCH_SCORING_BYTES").ok()?.parse().ok())
        .unwrap_or(200_000_000);
    let shape = match cli.shape.as_str() {
        "production" => LayeredShape::production_scale(cli.seed),
        "small" => LayeredShape::small(cli.seed),
        other => return Err(format!("unknown --shape '{other}' (production or small)")),
    };
    let base = layered_creature(&shape);
    let record_bytes = (shape.inputs + shape.outputs) * 4;
    let n_records = (bytes / record_bytes).max(1);
    let root = std::env::temp_dir().join(format!("cohort_bench-{}", std::process::id()));
    let result = run_in(cli, &root, &shape, &base, n_records, bytes);
    let _ = fs::remove_dir_all(&root);
    result
}

fn run_in(
    cli: &Cli,
    root: &Path,
    shape: &LayeredShape,
    base: &CreatureSpec,
    n_records: usize,
    bytes: usize,
) -> Result<(), String> {
    let data = root.join("data");
    fs::create_dir_all(&data).map_err(|e| e.to_string())?;
    // Several files, like a real corpus.
    let per_file = n_records.div_ceil(4);
    let mut left = n_records;
    let mut f = 0;
    while left > 0 {
        let take = per_file.min(left);
        let recs = corpus(shape.inputs, shape.outputs, take, cli.seed + f);
        fs::write(data.join(format!("part-{f:02}.bin")), to_le_bytes(&recs))
            .map_err(|e| e.to_string())?;
        left -= take;
        f += 1;
    }
    eprintln!(
        "base: {} inputs, {} non-input neurons, {} synapses; corpus {n_records} records ({bytes} B)",
        shape.inputs,
        base.neuron_count(),
        base.synapse_count()
    );

    for name in cli.cohorts.split(',').map(str::trim) {
        let members = match name {
            "graft" => graft_cohort(base, shape, 64, cli.seed + 1),
            "weight" => weight_change_cohort(base, 100, cli.seed + 2),
            "removal" => removal_cohort(base, shape, 100, cli.seed + 3),
            other => return Err(format!("unknown cohort '{other}'")),
        };
        let dir = root.join(name);
        write_cohort(&dir, &members).map_err(|e| e.to_string())?;

        let mut dir_ms = Vec::with_capacity(cli.repeats);
        let mut cohort_ms = Vec::with_capacity(cli.repeats);
        let mut stats = None;
        let mut max_rel = 0.0_f64;
        for _ in 0..cli.repeats {
            let t = Instant::now();
            let want = score_from_creature_dir_sampled(
                &dir,
                &data,
                GpuBackendLabel::CpuFallback,
                CostKind::Mse,
                &SampleSpec::full(),
            )?;
            dir_ms.push(t.elapsed().as_secs_f64() * 1e3);

            let t = Instant::now();
            let got = match score_from_creature_dir_cohort(
                &dir,
                &data,
                CostKind::Mse,
                &SampleSpec::full(),
            )? {
                CohortRun::Hosted { results, stats: s } => {
                    stats = Some(s);
                    results
                }
                CohortRun::Unhostable(r) => return Err(format!("{name}: cohort fell back: {r}")),
            };
            cohort_ms.push(t.elapsed().as_secs_f64() * 1e3);
            for (k, w) in &want {
                max_rel = max_rel.max((got[k].error - w.error).abs() / w.error.abs().max(1e-300));
            }
        }
        let stats = stats.expect("at least one repeat");
        let dir_median = median_ms(&mut dir_ms);
        let cohort_median = median_ms(&mut cohort_ms);
        let ci = |sorted: &[f64]| {
            median_ci95(sorted).map_or(serde_json::Value::Null, |(lo, hi, coverage)| {
                serde_json::json!({ "low": lo, "high": hi, "coverage": coverage })
            })
        };
        println!(
            "{}",
            serde_json::json!({
                "cohort": name,
                "creatures": stats.creatures,
                "records": n_records,
                "neuronActivationsPerRecord": {
                    "directory": stats.standalone_neurons,
                    "cohort": stats.union_neurons,
                    "ratio": stats.union_neurons as f64 / stats.standalone_neurons as f64,
                },
                "synapsesPerRecord": {
                    "directory": stats.standalone_synapses,
                    "cohort": stats.union_synapses,
                },
                "wallMs": {
                    "directory": { "median": dir_median, "ci95": ci(&dir_ms), "runs": dir_ms },
                    "cohort": { "median": cohort_median, "ci95": ci(&cohort_ms), "runs": cohort_ms },
                },
                "wallRatio": cohort_median / dir_median,
                "maxRelErrorVsDirectory": max_rel,
            })
        );
        fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.repeats == 0 {
        eprintln!("Error: --repeats must be at least 1");
        return ExitCode::FAILURE;
    }
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}
