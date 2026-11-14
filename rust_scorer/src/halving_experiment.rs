//! Native worker for the opt-in, fixed-population halving experiment.
//!
//! `scripts/bench-halving.py` freezes and verifies input hashes, counterbalances
//! arm order, bounds child execution and aggregates paired observations. This
//! worker reuses the directory loader and streaming decoder, then compares the
//! in-memory arms against an independent ordinary CPU directory scoring pass.
//! Packed replay timings exclude disk I/O; loading and audit costs are separate.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;
use neat_core::creature::compile_creature;
use neat_core::training_bin_stream::for_each_read_chunk;
use neat_core::training_data::find_bin_files;
use serde::Serialize;

use crate::corpus_guard::assert_records_aligned;
use crate::cost::CostKind;
use crate::gpu::GpuBackendLabel;
use crate::halving::{
    Arm, ArmOutcome, Audit, PackedCorpus, RungPlan, audit, run_cached_halving, run_full,
    run_independent_screening, run_nested_uncached, verify_full_match,
};
use crate::host_report::HostReport;
use crate::if_tree_fixture::{
    TreeSpec, records_to_le_bytes, tree_creature_json, tree_reference_output, varied_inputs,
};
use crate::multi_score::{load_creatures_from_dir, score_from_creature_dir};
use crate::read_tuning::training_read_target_bytes_from_env;
use crate::sampling::SampleSpec;
use crate::stream_io::run_io_loop;
use crate::stream_score::effective_fused_read_buf_len;

const SCHEMA: &str = "neat-halving-observation/1";

#[derive(Debug, Parser)]
#[command(name = "halving_bench", version)]
struct Cli {
    #[arg(required_unless_present = "generate_fixture")]
    creatures_dir: Option<PathBuf>,
    #[arg(required_unless_present = "generate_fixture")]
    data_dir: Option<PathBuf>,
    /// Create a new, public synthetic correctness fixture and print its driver config.
    #[arg(long, conflicts_with_all = ["creatures_dir", "data_dir"])]
    generate_fixture: Option<PathBuf>,
    #[arg(long, default_value = "MSE")]
    cost: CostKind,
    #[arg(long, default_value_t = 0)]
    seed: u64,
    /// A permutation of full,stride,nested,cached; set by the paired driver.
    #[arg(long, default_value = "full,stride,nested,cached")]
    order: String,
    /// The replay materialises the corpus; refuse a larger input, never truncate.
    #[arg(long, default_value_t = 268_435_456)]
    max_corpus_bytes: u64,
    /// Conservative upper bound including the independent full-data audit.
    #[arg(long, default_value_t = 1_000_000_000)]
    max_record_evaluations: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Rejected {
    candidate: usize,
    rung: usize,
    full_error: f64,
    better_than_selected: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    outcome: ArmOutcome,
    total_secs: f64,
    clone_secs: f64,
    audit: Audit,
    rejected: Vec<Rejected>,
    max_relative_deviation: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    schema: &'static str,
    status: &'static str,
    scorer_version: &'static str,
    cost: &'static str,
    seed: u64,
    order: Vec<String>,
    candidates: Vec<String>,
    records: usize,
    corpus_bytes: u64,
    load_candidate_secs: f64,
    compile_secs: f64,
    load_corpus_secs: f64,
    corpus_capacity_bytes: usize,
    read_buffer_bytes: usize,
    process_peak_rss_bytes: Option<u64>,
    authoritative_secs: f64,
    authoritative_record_evaluations: u64,
    authoritative_errors: Vec<f64>,
    audit_comparison_secs: f64,
    total_record_evaluations: u64,
    total_secs: f64,
    host: HostReport,
    arms: Vec<Observation>,
}

/// Entry point of the separate `halving_bench` binary.
/// Failures emit a JSON diagnostic and a non-zero exit, never a success report.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let start = Instant::now();
    let result = if let Some(root) = &cli.generate_fixture {
        generate_fixture(root)
    } else {
        run(&cli).and_then(|r| serde_json::to_value(r).map_err(|e| e.to_string()))
    };
    match result {
        Ok(report) => match serde_json::to_string(&report) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("halving_bench: report serialisation failed: {e}");
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            println!(
                "{}",
                serde_json::json!({
                    "schema": SCHEMA, "status": "failed", "error": e,
                    "elapsedSecs": start.elapsed().as_secs_f64(),
                    "completedRecordEvaluations": null
                })
            );
            eprintln!("halving_bench: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<Report, String> {
    let started = Instant::now();
    if cli.seed > u32::MAX as u64 {
        return Err("seed must be in [0, 2^32-1] for stride index precision".to_string());
    }
    let creatures_dir = cli
        .creatures_dir
        .as_ref()
        .expect("clap requires candidate directory");
    let data_dir = cli.data_dir.as_ref().expect("clap requires data directory");
    let order: Vec<String> = cli.order.split(',').map(str::to_owned).collect();
    let expected = BTreeSet::from(["cached", "full", "nested", "stride"]);
    if order.len() != 4 || order.iter().map(String::as_str).collect::<BTreeSet<_>>() != expected {
        return Err("--order must be a permutation of full,stride,nested,cached".to_string());
    }
    let t = Instant::now();
    let loaded = load_creatures_from_dir(creatures_dir)?;
    if loaded.iter().any(|c| !c.creature.forward_only) {
        return Err("this fixed-population experiment requires forwardOnly=true".to_string());
    }
    let keys: Vec<String> = loaded.iter().map(|c| c.key.clone()).collect();
    if keys.iter().collect::<BTreeSet<_>>().len() != keys.len() {
        return Err("candidate filename stems must be unique after trimming".to_string());
    }
    let load_candidate_secs = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let networks = loaded
        .iter()
        .map(|c| compile_creature(&c.creature).map_err(|e| format!("compile {}: {e}", c.key)))
        .collect::<Result<Vec<_>, _>>()?;
    let compile_secs = t.elapsed().as_secs_f64();
    let num_inputs = loaded[0].creature.input;
    let num_outputs = loaded[0].creature.output;
    let vpr = num_inputs
        .checked_add(num_outputs)
        .ok_or("record width overflow")?;
    let record_bytes = vpr.checked_mul(4).ok_or("record byte width overflow")?;
    let t = Instant::now();
    let files = find_bin_files(data_dir).map_err(|e| format!("find corpus: {e}"))?;
    if files.is_empty() {
        return Err("no .bin training files".to_string());
    }
    assert_records_aligned(&files, record_bytes)?;
    let mut corpus_bytes = 0_u64;
    for path in &files {
        let bytes = std::fs::metadata(path)
            .map_err(|e| format!("corpus metadata: {e}"))?
            .len();
        corpus_bytes = corpus_bytes
            .checked_add(bytes)
            .ok_or("corpus size overflow")?;
    }
    if corpus_bytes > cli.max_corpus_bytes {
        return Err(format!(
            "corpus byte limit exceeded: {corpus_bytes} > {}",
            cli.max_corpus_bytes
        ));
    }
    let records =
        usize::try_from(corpus_bytes / record_bytes as u64).map_err(|_| "record count overflow")?;
    if records == 0 {
        return Err("corpus has zero records".to_string());
    }
    // Four arms each cost at most 1.22 * full plus one full audit: < 6 * full.
    let upper_evals = (records as u64)
        .checked_mul(networks.len() as u64)
        .and_then(|v| v.checked_mul(6))
        .ok_or("evaluation count overflow")?;
    if upper_evals > cli.max_record_evaluations {
        return Err(format!(
            "record evaluation limit exceeded: upper bound {upper_evals} > {}",
            cli.max_record_evaluations
        ));
    }
    let read_buffer_bytes = effective_fused_read_buf_len(
        record_bytes,
        training_read_target_bytes_from_env(record_bytes),
    );
    let mut packed = Vec::with_capacity(
        usize::try_from(corpus_bytes / 4).map_err(|_| "corpus capacity overflow")?,
    );
    let (mut pending, mut head, mut floats) = (Vec::new(), 0, Vec::new());
    let mut sampler = SampleSpec::full().sampler();
    for_each_read_chunk(&files, read_buffer_bytes, |bytes| {
        run_io_loop(
            bytes,
            &mut pending,
            &mut head,
            &mut floats,
            record_bytes,
            &mut sampler,
            &mut |chunk, _| {
                if packed
                    .len()
                    .checked_add(chunk.len())
                    .is_none_or(|n| n as u64 > corpus_bytes / 4)
                {
                    return Err("corpus grew while loading".to_string());
                }
                packed.extend_from_slice(chunk);
                Ok(())
            },
        )
    })?;
    if packed.len() as u64 != corpus_bytes / 4 || !pending.is_empty() {
        return Err("corpus size changed while loading".to_string());
    }
    let load_corpus_secs = t.elapsed().as_secs_f64();
    drop((pending, floats));
    let corpus = PackedCorpus {
        records: &packed,
        num_inputs,
        num_outputs,
        forward_only: true,
    };
    let plan = RungPlan::issue_680_default();
    let mut outcomes = Vec::new();
    for name in &order {
        let start = Instant::now();
        let mut nets = networks.clone();
        let clone_secs = start.elapsed().as_secs_f64();
        let outcome = match name.as_str() {
            "full" => run_full(cli.cost, &mut nets, corpus),
            "stride" => run_independent_screening(cli.cost, &mut nets, corpus, &plan, cli.seed),
            "nested" => run_nested_uncached(cli.cost, &mut nets, corpus, &plan, cli.seed),
            "cached" => run_cached_halving(cli.cost, &mut nets, corpus, &plan, cli.seed),
            _ => unreachable!("validated permutation"),
        }
        .map_err(|e| format!("{name}: {e}"))?;
        outcomes.push((outcome, start.elapsed().as_secs_f64(), clone_secs));
    }
    // Independent production loading, chunking and worker aggregation remain authoritative.
    let t = Instant::now();
    let authoritative = score_from_creature_dir(
        creatures_dir,
        data_dir,
        GpuBackendLabel::CpuFallback,
        cli.cost,
    )?;
    let authoritative_secs = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let mut full = outcomes
        .iter()
        .find(|(o, _, _)| o.arm == Arm::Full)
        .expect("full ran")
        .0
        .clone();
    for (i, key) in keys.iter().enumerate() {
        let value = authoritative
            .get(key)
            .ok_or_else(|| format!("authoritative scorer omitted {key}"))?;
        if value.record_count != records {
            return Err(format!("authoritative record count changed for {key}"));
        }
        full.final_errors[i] = Some(value.error);
    }
    let mut arms = Vec::new();
    for (outcome, total_secs, clone_secs) in outcomes {
        let max_relative_deviation = verify_full_match(&outcome, &full)?;
        let a = audit(&outcome, &full)?;
        let rejected = outcome
            .eliminated_at
            .iter()
            .enumerate()
            .filter_map(|(i, rung)| {
                rung.map(|rung| {
                    let error = full.final_errors[i].expect("authority populated");
                    Rejected {
                        candidate: i,
                        rung,
                        full_error: error,
                        better_than_selected: error < a.selected_full_error,
                    }
                })
            })
            .collect();
        arms.push(Observation {
            outcome,
            total_secs,
            clone_secs,
            audit: a,
            rejected,
            max_relative_deviation,
        });
    }
    let authoritative_record_evaluations = records as u64 * networks.len() as u64;
    let total_record_evaluations = arms
        .iter()
        .map(|a| a.outcome.record_evaluations)
        .sum::<u64>()
        + authoritative_record_evaluations;
    let audit_comparison_secs = t.elapsed().as_secs_f64();
    let host = HostReport::resolve(record_bytes);
    let process_peak_rss_bytes = peak_rss_bytes()?;
    Ok(Report {
        schema: SCHEMA,
        status: "ok",
        scorer_version: env!("CARGO_PKG_VERSION"),
        cost: cli.cost.as_str(),
        seed: cli.seed,
        order,
        candidates: keys,
        records,
        corpus_bytes,
        load_candidate_secs,
        compile_secs,
        load_corpus_secs,
        corpus_capacity_bytes: packed.capacity() * size_of::<f32>(),
        read_buffer_bytes,
        process_peak_rss_bytes,
        authoritative_secs,
        authoritative_record_evaluations,
        authoritative_errors: full
            .final_errors
            .iter()
            .map(|e| e.expect("authority populated"))
            .collect(),
        audit_comparison_secs,
        total_record_evaluations,
        total_secs: started.elapsed().as_secs_f64(),
        host,
        arms,
    })
}

fn peak_rss_bytes() -> Result<Option<u64>, String> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: getrusage writes a complete rusage to this valid output pointer.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
            return Err(format!(
                "peak RSS probe failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: the successful getrusage call initialised the structure.
        let rss = unsafe { usage.assume_init() }.ru_maxrss;
        let rss = u64::try_from(rss).map_err(|_| "peak RSS probe returned a negative value")?;
        #[cfg(target_os = "linux")]
        let rss = rss
            .checked_mul(1024)
            .ok_or("peak RSS byte count overflow")?;
        Ok(Some(rss))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Ok(None)
    }
}

fn generate_fixture(root: &std::path::Path) -> Result<serde_json::Value, String> {
    let started = Instant::now();
    // Refuse existing state: this command owns only a newly created directory.
    std::fs::create_dir(root).map_err(|e| format!("create synthetic root: {e}"))?;
    let root = root
        .canonicalize()
        .map_err(|e| format!("resolve synthetic root: {e}"))?;
    let mut cases = Vec::new();
    for (split, base, records) in [("calibration", 0, 2048), ("validation", 100_000, 4096)] {
        for population in [12, 24] {
            let id = format!("{split}-{population}");
            let case = root.join(&id);
            let candidates = case.join("candidates");
            let data = case.join("data");
            std::fs::create_dir(&case)
                .and_then(|()| std::fs::create_dir(&candidates))
                .and_then(|()| std::fs::create_dir(&data))
                .map_err(|e| format!("create fixture: {e}"))?;
            for i in 0..population {
                let spec = TreeSpec::new(4, 2, base + i as u64);
                std::fs::write(
                    candidates.join(format!("candidate-{i:03}.json")),
                    tree_creature_json(&spec),
                )
                .map_err(|e| format!("write fixture candidate: {e}"))?;
            }
            let oracle = TreeSpec::new(4, 2, base + (population / 2) as u64);
            let mut packed = Vec::new();
            for r in 0..records {
                let inputs = varied_inputs(4, base as usize + population * 10_000 + r);
                packed.extend_from_slice(&inputs);
                packed.push(tree_reference_output(&oracle, &inputs));
            }
            std::fs::write(data.join("0.bin"), records_to_le_bytes(&packed))
                .map_err(|e| format!("write fixture corpus: {e}"))?;
            cases.push(
                serde_json::json!({"id": id, "split": split, "kind": "synthetic",
                "creaturesDir": candidates, "dataDir": data}),
            );
        }
    }
    Ok(
        serde_json::json!({"schema": "neat-halving-config/1", "cases": cases,
        "seeds": [0, 17, 101], "repetitions": 8, "cost": "MSE",
        "splitJustification": "Distinct synthetic candidate seeds and disjoint input-index ranges; no learned parameters or threshold tuning.",
        "inputPreparationSecs": started.elapsed().as_secs_f64(),
        "limits": {"maxCorpusBytes": 268435456, "maxRecordEvaluations": 1000000000,
            "trialSeconds": 120, "totalSeconds": 1800}}),
    )
}
