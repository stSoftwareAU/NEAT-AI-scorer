//! Per-cost CPU throughput bench — Issue #124.
//!
//! Companion to `float_scan_bench` that drives the full forward-only fused
//! scoring path
//! ([`rust_scorer::stream_score::accumulate_cost_sum_forward_only_fused`]) for
//! every supported [`rust_scorer::cost::CostKind`] against a single creature
//! and a `.bin` corpus. Used by Issue #124 to record the per-cost CPU
//! baseline so each non-MSE cost gets the bench-and-decide step the parent
//! issue (#119) defers GPU kernel work behind.
//!
//! Issue #134 unblocked `CATEGORICAL_ERROR` after
//! `categorical_error_sum_batch_packed` landed via `NEAT-AI-core#88`; the
//! bench now measures all seven built-in costs in a single sweep. The
//! `skipped` array remains in the JSON contract for forwards-compatibility
//! (future kernel-only costs may legitimately surface a dispatch error
//! here) but is empty under normal operation.
//!
//! Output is a single JSON object on stdout, suitable for piping into
//! `jq`/notebooks and for pasting straight into the issue thread. Build:
//!
//! ```text
//! cargo build --release -p rust_scorer --bin cost_scan_bench
//! ./target/release/cost_scan_bench <creature.json> <data_dir> --runs 5
//! ```
//!
//! Both `--runs` and the corpus size affect how stable the median is — the
//! existing `float_scan_bench` defaults to 7 runs, this bin defaults to 5 so
//! a 6-cost sweep stays within ~1 minute on a small corpus.

// Issue #470 dead-code audit: the `env_tuning` / `read_tuning` module copies
// `float_scan_bench` needs were declared here too but never referenced — this
// bench drives the library entry point
// `rust_scorer::stream_score::accumulate_cost_sum_forward_only_fused`, which
// applies the crate's own read tuning internally. Both `mod` declarations (and
// the `#[allow(dead_code)]` attributes that masked them) have been removed.

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use neat_core::creature::{compile_creature, parse_creature_json};
use neat_core::training_data::{TrainingDataConfig, find_bin_files};

use rust_scorer::cost::CostKind;
use rust_scorer::stream_score::accumulate_cost_sum_forward_only_fused;

#[derive(Parser, Debug)]
#[command(name = "cost_scan_bench")]
struct Cli {
    /// Path to the creature JSON file (forward-only).
    creature: PathBuf,

    /// Directory containing `.bin` training files (numeric sort,
    /// same as the production scorer).
    data_dir: PathBuf,

    /// Number of timed iterations per cost (median reported).
    #[arg(long, default_value_t = 5)]
    runs: usize,
}

/// Result of one timed run for a single cost.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CostRow {
    cost: &'static str,
    median_ms: f64,
    times_ms: Vec<f64>,
    records: usize,
    records_per_sec: f64,
    checksum: f64,
}

#[derive(serde::Serialize)]
struct SkippedRow {
    cost: &'static str,
    reason: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    creature: PathBuf,
    data_dir: PathBuf,
    file_count: usize,
    total_bytes: u64,
    num_inputs: usize,
    num_outputs: usize,
    runs: usize,
    rows: Vec<CostRow>,
    skipped: Vec<SkippedRow>,
}

fn median_ms(times: &mut [f64]) -> f64 {
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mid = times.len() / 2;
    if times.len() % 2 == 1 {
        times[mid]
    } else {
        (times[mid - 1] + times[mid]) / 2.0
    }
}

fn run_one_cost(
    cost: CostKind,
    bin_files: &[PathBuf],
    config: &TrainingDataConfig,
    creature_json: &str,
    runs: usize,
) -> Result<CostRow, String> {
    let creature = parse_creature_json(creature_json).map_err(|e| e.to_string())?;
    // Warm-up run: validates the cost is dispatchable and primes caches.
    let mut net = compile_creature(&creature).map_err(|e| e.to_string())?;
    let (warm_sum, warm_records, _, _, _) =
        accumulate_cost_sum_forward_only_fused(cost, bin_files, config, &mut net)?;

    let mut times = Vec::with_capacity(runs);
    let mut checksum = warm_sum;
    let mut records = warm_records;
    for _ in 0..runs {
        // Re-compile per iteration so the network state is identical between
        // runs — matches the Criterion `iter_batched` pattern in
        // `benches/scoring.rs::bench_score_from_json_fused`.
        let mut net = compile_creature(&creature).map_err(|e| e.to_string())?;
        let t0 = Instant::now();
        let (sum, n_records, _, _, _) =
            accumulate_cost_sum_forward_only_fused(cost, bin_files, config, &mut net)?;
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        times.push(elapsed_ms);
        checksum = sum;
        records = n_records;
    }

    let med = median_ms(&mut times.clone());
    let rps = if med > 0.0 {
        (records as f64) / (med / 1000.0)
    } else {
        0.0
    };
    Ok(CostRow {
        cost: cost.as_str(),
        median_ms: (med * 100.0).round() / 100.0,
        times_ms: times
            .into_iter()
            .map(|t| (t * 100.0).round() / 100.0)
            .collect(),
        records,
        records_per_sec: (rps * 100.0).round() / 100.0,
        checksum,
    })
}

fn main() {
    let cli = Cli::parse();

    let creature_json = match fs::read_to_string(&cli.creature) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read {}: {e}", cli.creature.display());
            std::process::exit(2);
        }
    };
    let creature = match parse_creature_json(&creature_json) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to parse creature {}: {e}", cli.creature.display());
            std::process::exit(2);
        }
    };

    if !cli.data_dir.is_dir() {
        eprintln!("data_dir does not exist: {}", cli.data_dir.display());
        std::process::exit(2);
    }
    let bin_files = match find_bin_files(&cli.data_dir) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "Failed to list bin files in {}: {e}",
                cli.data_dir.display()
            );
            std::process::exit(2);
        }
    };
    if bin_files.is_empty() {
        eprintln!("No .bin files in {}", cli.data_dir.display());
        std::process::exit(2);
    }

    let num_inputs = creature.input;
    let num_outputs = creature.output;
    let config = TrainingDataConfig {
        num_inputs,
        num_outputs,
    };

    let total_bytes: u64 = bin_files
        .iter()
        .map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .sum();

    // Costs we measure. Since Issue #134 every built-in cost dispatches
    // through `accumulate_cost_sum`; the `skipped` array stays in the
    // JSON contract for forwards-compatibility (a future kernel-only
    // cost could legitimately surface a dispatch error here) but is
    // empty under normal operation.
    let candidates = [
        CostKind::Mse,
        CostKind::Mae,
        CostKind::Mape,
        CostKind::Msle,
        CostKind::Hinge,
        CostKind::CrossEntropy,
        CostKind::CategoricalError,
    ];

    let mut rows: Vec<CostRow> = Vec::new();
    let mut skipped: Vec<SkippedRow> = Vec::new();
    for cost in candidates {
        match run_one_cost(cost, &bin_files, &config, &creature_json, cli.runs) {
            Ok(row) => rows.push(row),
            Err(e) => skipped.push(SkippedRow {
                cost: cost.as_str(),
                reason: e,
            }),
        }
    }

    let report = Report {
        creature: cli.creature,
        data_dir: cli.data_dir,
        file_count: bin_files.len(),
        total_bytes,
        num_inputs,
        num_outputs,
        runs: cli.runs,
        rows,
        skipped,
    };

    // Single-line JSON keeps the output trivially parseable.
    println!(
        "{}",
        serde_json::to_string(&report).expect("serialise report")
    );
}
