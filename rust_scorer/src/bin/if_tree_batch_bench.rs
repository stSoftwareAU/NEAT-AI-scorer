//! Tree-heavy candidate-batching bench — Issue #574.
//!
//! `NEAT-AI-Forests` evaluates **many candidate grafts against the same
//! corpus**: one sweep of the training data, N `IF`-heavy decision-tree
//! creatures scored in that sweep, ranked by loss. This bench measures exactly
//! that shape and reports the two rates Forests plans against —
//! **candidates/second** and **records/second** — plus the product
//! (candidate-record evaluations per second), which is the figure that stays
//! comparable when the batch size changes.
//!
//! The fixture is generated in a temporary directory from
//! [`rust_scorer::if_tree_fixture`], so the bench needs no committed creature or
//! corpus (this repo ships neither) and is reproducible on any host:
//!
//! ```text
//! cargo build --release -p rust_scorer --bin if_tree_batch_bench
//! ./target/release/if_tree_batch_bench --candidates 64 --records 200000 --depth 3
//! ```
//!
//! Output is a single JSON object on stdout, ready to paste into an issue or
//! pipe through `jq`. The bench is **fail-loud**: an unwritable fixture, a
//! failed scoring run, or `--gpu on` with no adapter exits non-zero rather than
//! reporting an empty result as success.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use clap::Parser;

use rust_scorer::cost::CostKind;
use rust_scorer::gpu::{GpuBackendLabel, GpuMode, select_adapter};
use rust_scorer::if_tree_fixture::{
    TreeSpec, corpus_records, grafted_creature_json, records_to_le_bytes, tree_creature_json,
};
use rust_scorer::multi_score::{score_from_creature_dir, score_from_creature_dir_gpu};

#[derive(Parser, Debug)]
#[command(name = "if_tree_batch_bench")]
struct Cli {
    /// Candidate decision trees scored against the shared corpus.
    #[arg(long, default_value_t = 64)]
    candidates: usize,

    /// Records in the generated corpus.
    #[arg(long, default_value_t = 100_000)]
    records: usize,

    /// Depth of each candidate tree (1 = stump).
    #[arg(long, default_value_t = 3)]
    depth: u32,

    /// Input columns per record.
    #[arg(long, default_value_t = 8)]
    inputs: usize,

    /// Timed repetitions; the median is reported.
    #[arg(long, default_value_t = 3)]
    runs: usize,

    /// Every Nth candidate is a large creature carrying an appended IF
    /// correction graft (0 disables). Mixes the scratch kernel into the batch.
    #[arg(long, default_value_t = 8)]
    graft_every: usize,

    /// Hidden width of the grafted candidates.
    #[arg(long, default_value_t = 288)]
    graft_hidden: usize,

    /// GPU mode: `auto` uses a GPU when one is present, `on` requires one,
    /// `off` forces the CPU pipeline.
    #[arg(long, default_value = "auto")]
    gpu: GpuMode,

    /// Keep the generated fixture directory instead of deleting it.
    #[arg(long, default_value_t = false)]
    keep_fixture: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchReport {
    candidates: usize,
    records: usize,
    depth: u32,
    inputs: usize,
    grafted_candidates: usize,
    runs: usize,
    gpu_backend: &'static str,
    median_ms: f64,
    times_ms: Vec<f64>,
    candidates_per_sec: f64,
    records_per_sec: f64,
    candidate_record_evaluations_per_sec: f64,
    best_candidate: String,
    best_error: f64,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(report) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("report serialises")
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("if_tree_batch_bench: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<BenchReport, String> {
    if cli.candidates == 0 || cli.records == 0 || cli.runs == 0 {
        return Err("--candidates, --records and --runs must all be non-zero".to_string());
    }

    let root = std::env::temp_dir().join(format!("if_tree_batch_bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let fixture = write_fixture(cli, &root)?;

    let result = bench(cli, &fixture);
    if !cli.keep_fixture {
        // Best-effort cleanup; a failure here must not mask the bench result.
        let _ = std::fs::remove_dir_all(&root);
    } else {
        eprintln!("fixture kept at {}", root.display());
    }
    result
}

/// Generated fixture paths plus the grafted-candidate count.
struct Fixture {
    creatures_dir: PathBuf,
    data_dir: PathBuf,
    grafted: usize,
}

fn write_fixture(cli: &Cli, root: &Path) -> Result<Fixture, String> {
    let creatures_dir = root.join("creatures");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&creatures_dir)
        .map_err(|e| format!("cannot create {}: {e}", creatures_dir.display()))?;
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("cannot create {}: {e}", data_dir.display()))?;

    let mut grafted = 0usize;
    for c in 0..cli.candidates {
        let spec = TreeSpec::new(cli.inputs, cli.depth, 1_000 + c as u64);
        let is_graft = cli.graft_every > 0 && c % cli.graft_every == cli.graft_every - 1;
        let json = if is_graft {
            grafted += 1;
            grafted_creature_json(&spec, cli.graft_hidden)
        } else {
            tree_creature_json(&spec)
        };
        let path = creatures_dir.join(format!("candidate-{c:05}.json"));
        std::fs::write(&path, json).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }

    // Targets are an oracle tree's own predictions, so candidate losses spread
    // out and the ranking the bench reports is meaningful.
    let oracle = TreeSpec::new(cli.inputs, cli.depth, 0);
    let records = corpus_records(&oracle, cli.records);
    let bin = data_dir.join("0.bin");
    std::fs::write(&bin, records_to_le_bytes(&records))
        .map_err(|e| format!("cannot write {}: {e}", bin.display()))?;

    Ok(Fixture {
        creatures_dir,
        data_dir,
        grafted,
    })
}

fn bench(cli: &Cli, fixture: &Fixture) -> Result<BenchReport, String> {
    let ctx = match cli.gpu {
        GpuMode::Off => None,
        GpuMode::Auto | GpuMode::On => match select_adapter() {
            Ok(Some(c)) if c.backend != GpuBackendLabel::CpuFallback => Some(Arc::new(c)),
            Ok(_) | Err(_) if cli.gpu == GpuMode::On => {
                return Err("--gpu on requested but no compatible adapter was found".to_string());
            }
            Ok(_) => None,
            Err(e) => return Err(format!("GPU adapter selection failed: {e}")),
        },
    };
    let backend = ctx
        .as_ref()
        .map_or(GpuBackendLabel::CpuFallback, |c| c.backend);

    let mut times_ms = Vec::with_capacity(cli.runs);
    let mut best = (String::new(), f64::INFINITY);
    for _ in 0..cli.runs {
        let started = Instant::now();
        let scores = match ctx.as_ref() {
            Some(ctx) => score_from_creature_dir_gpu(
                &fixture.creatures_dir,
                &fixture.data_dir,
                backend,
                Arc::clone(ctx),
                1,
                CostKind::Mse,
            ),
            None => score_from_creature_dir(
                &fixture.creatures_dir,
                &fixture.data_dir,
                backend,
                CostKind::Mse,
            ),
        }
        .map_err(|e| format!("scoring run failed: {e}"))?;
        times_ms.push(started.elapsed().as_secs_f64() * 1_000.0);

        if scores.len() != cli.candidates {
            return Err(format!(
                "expected {} candidate scores, got {}",
                cli.candidates,
                scores.len()
            ));
        }
        for (name, result) in &scores {
            if result.error < best.1 {
                best = (name.clone(), result.error);
            }
        }
    }

    let median_ms = median(&times_ms);
    if median_ms <= 0.0 || !median_ms.is_finite() {
        return Err(format!("unusable median run time: {median_ms} ms"));
    }
    let secs = median_ms / 1_000.0;
    let candidates = cli.candidates as f64;
    let records = cli.records as f64;

    Ok(BenchReport {
        candidates: cli.candidates,
        records: cli.records,
        depth: cli.depth,
        inputs: cli.inputs,
        grafted_candidates: fixture.grafted,
        runs: cli.runs,
        gpu_backend: backend.as_str(),
        median_ms,
        times_ms,
        candidates_per_sec: candidates / secs,
        records_per_sec: records / secs,
        candidate_record_evaluations_per_sec: candidates * records / secs,
        best_candidate: best.0,
        best_error: best.1,
    })
}

fn median(times_ms: &[f64]) -> f64 {
    let mut sorted = times_ms.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}
