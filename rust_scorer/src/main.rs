//! Rust CLI scorer for NEAT-AI creatures.
//!
//! Minimal tool: full-dataset **MSE** error and the same fitness **score** formula
//! as TypeScript (`Score.ts`). Reads `input`, `output`, optional `forwardOnly`,
//! and `semanticVersion` from the creature JSON only — no separate I/O flags.
//!
//! When `"forwardOnly": true`, uses self-tuned chunked reads plus
//! `mse_sum_batch_packed` (fused path). Otherwise uses a streaming iterator with
//! per-record activation (slower; for recurrent / non-forward-only exports).
//!
//! Issue #1967 - Build Rust CLI scorer application.

mod cost;
mod env_tuning;
mod gpu;
mod multi_score;
mod read_tuning;
mod sampling;
mod scoring;
mod stream_io;
mod stream_score;

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Instant;

use clap::Parser;
use neat_core::creature::{compile_creature, parse_creature_json};
use neat_core::training_data::{TrainingDataConfig, TrainingDataIterator, find_bin_files};

use std::sync::Arc;

use crate::cost::CostKind;
use crate::gpu::{GpuBackendLabel, GpuMode, ScoringPath};
use crate::multi_score::{score_from_creature_dir_gpu_sampled, score_from_creature_dir_sampled};
use crate::read_tuning::{training_read_backend_label, training_read_target_bytes_from_env};
use crate::sampling::{SampleSpec, parse_sample_rate};
use crate::scoring::{ScoreResult, calculate_score, compute_score_components};
use crate::stream_score::activation_worker_count_for_scorer;

/// Matches `DEFAULT_COST_OF_GROWTH` in `src/config/NeatConfig.ts` (CLI is KISS: no flag).
const GROWTH_COST: f64 = 0.000_000_1;

/// Full-dataset fitness score for a NEAT creature (native, minimal CLI).
///
/// Two input modes (positional contract unchanged):
/// * default: `rust_scorer <creature.json> <data_dir>`
/// * stdin:   `rust_scorer --creature-stdin <data_dir>` (creature JSON on stdin)
///
/// Loss is `MSE` by default; pick another with `--cost <NAME>` (see the
/// README "Cost function selector" section for the full list and examples).
#[derive(Parser, Debug)]
#[command(
    name = "rust_scorer",
    about = "Full-dataset fitness score for a NEAT creature (fast native path; see README for --cost names)",
    arg_required_else_help = true
)]
struct Cli {
    /// Read the creature JSON from stdin instead of a file.
    ///
    /// When set, the only positional argument is `<data_dir>`; useful for
    /// restricted worker/sandbox environments where `Deno.makeTempFile`
    /// (or similar) may fail even with write permission granted.
    #[arg(long)]
    creature_stdin: bool,

    /// GPU mode (Issue #80, default flipped to `auto` in Issue #83).
    ///
    /// * `auto` — **default**. Probe for a compatible GPU; silently fall
    ///   back to CPU when none is found, when the GPU kernel cannot host
    ///   the loaded creatures, or when the scoring path's bench evidence
    ///   does not support GPU (see [`auto_should_use_gpu`]). For the
    ///   directory path a CPU-only pre-flight (Issue #180) checks the set
    ///   against the 256-neuron shader cap *before* a GPU device is
    ///   created, so an unhostable set falls back cleanly (valid JSON,
    ///   exit 0) without ever building a `wgpu` context.
    /// * `on`   — require a compatible GPU; non-zero exit if none found.
    /// * `off`  — skip GPU detection entirely; run the CPU pipeline.
    ///
    /// Falls back to the `NEAT_SCORER_GPU` env var when not provided.
    #[arg(long, value_enum, value_name = "MODE")]
    gpu: Option<GpuMode>,

    /// Built-in cost function (Issues #120, #121).
    ///
    /// Names match the TypeScript `BUILT_IN_COST_NAMES` strings exactly —
    /// see `NEAT-AI/src/Costs.ts`. Defaults to `MSE`, preserving the
    /// historical scoring behaviour. There is **no** environment-variable
    /// override (KISS); unknown values are rejected by clap with a
    /// non-zero exit and a stderr message listing the supported set.
    ///
    /// Dispatch is wired (#121, #134): the fused forward-only path and
    /// the per-record recurrent path both call `accumulate_cost_sum`,
    /// so every one of `MSE`, `RMSE`, `MAE`, `MAPE`, `MSLE`, `HINGE`,
    /// `CROSS_ENTROPY` and `CATEGORICAL_ERROR` computes the requested
    /// loss — the last one unblocked in #134 after the upstream
    /// `categorical_error_sum_batch_packed` helper landed via
    /// `NEAT-AI-core#88`.
    ///
    /// GPU support: the batched/scratch kernels host `MSE`, `RMSE` and `MAE`
    /// on one shared forward pass. `MSE`/`RMSE` accumulate squared error
    /// (`RMSE` adds only a host-side `sqrt` at finalisation, Issue #339); `MAE`
    /// accumulates absolute error (Issue #316), selected per dispatch by the
    /// shader's `cost_kind` header field. The remaining costs under `--gpu auto`
    /// silently fall back to the CPU pipeline; under `--gpu on` they hard-error
    /// before any scoring runs.
    ///
    /// See the README "Cost function selector" section for per-cost
    /// usage examples.
    #[arg(long, value_enum, value_name = "NAME", default_value_t = CostKind::default())]
    cost: CostKind,

    /// Record-level sub-sampling rate for the forward-only streaming reader
    /// (Issue #310, multi-fidelity fitness).
    ///
    /// A value in `(0, 1]`; defaults to `1` (score every record). When `< 1`
    /// the reader deterministically keeps a stratified subsample of the corpus
    /// — record `i` is kept iff `floor((i+1)·rate) > floor(i·rate)` — cutting
    /// the scored record count (and wall-clock) roughly proportionally with **no
    /// second corpus on disk**. The stride matches the TypeScript consumer
    /// (NEAT-AI#3257) so both agree on which records survive. Out-of-range
    /// values are rejected with a non-zero exit (never silently clamped).
    ///
    /// The reported `error`/`score` are then computed over the sampled subset,
    /// `recordCount` is the number of records actually scored, and `sampleRate`
    /// echoes the effective rate so a caller can confirm it was honoured.
    #[arg(long, value_name = "RATE", value_parser = parse_sample_rate)]
    sample_rate: Option<f64>,

    /// Stride phase offset for `--sample-rate` (Issue #310).
    ///
    /// Shifts the deterministic stratified stride to select a *different*
    /// subsample of the same size — e.g. rotating the sampled stratum per
    /// generation — without any randomness. Defaults to `0`. Ignored when
    /// `--sample-rate` is `1` or absent.
    #[arg(long, value_name = "PHASE", default_value_t = 0)]
    sample_phase: u64,

    /// Positional arguments.
    ///
    /// * default mode: `<creature.json> <data_dir>` (two values).
    /// * `--creature-stdin` mode: `<data_dir>` (one value).
    #[arg(num_args = 1..=2, value_name = "ARGS")]
    args: Vec<PathBuf>,
}

/// Resolve the `(creature_json, data_dir)` pair from parsed CLI args.
///
/// In stdin mode this blocks reading from `std::io::stdin` until EOF. The
/// positional argument count is validated here rather than in clap so the
/// error message can describe the chosen input mode.
fn resolve_inputs(cli: &Cli) -> Result<(String, PathBuf), String> {
    if cli.creature_stdin {
        if cli.args.len() != 1 {
            return Err(
                "With --creature-stdin, exactly one positional argument is required: <data_dir>"
                    .to_string(),
            );
        }
        let mut creature_json = String::new();
        std::io::stdin()
            .read_to_string(&mut creature_json)
            .map_err(|e| format!("Failed to read creature JSON from stdin: {e}"))?;
        if creature_json.trim().is_empty() {
            return Err("Creature JSON from stdin is empty".to_string());
        }
        Ok((creature_json, cli.args[0].clone()))
    } else {
        if cli.args.len() != 2 {
            return Err(
                "Expected arguments: <creature.json> <data_dir> (or use --creature-stdin)"
                    .to_string(),
            );
        }
        let creature_path = &cli.args[0];
        let creature_json = fs::read_to_string(creature_path).map_err(|e| {
            format!(
                "Failed to read creature file '{}': {e}",
                creature_path.display()
            )
        })?;
        Ok((creature_json, cli.args[1].clone()))
    }
}

enum RunOutput {
    // Issue #121: `ScoreResult` now also carries `cost_name: String`. The
    // total variant size crossed clippy's `large_enum_variant` threshold,
    // so box the single-creature payload — `Multi` is already a `BTreeMap`
    // (one heap pointer) and stays small.
    Single(Box<ScoreResult>),
    Multi(BTreeMap<String, ScoreResult>),
}

fn run(cli: &Cli) -> Result<RunOutput, String> {
    // Resolve the GPU backend label up-front. For `--gpu off` this is a
    // constant `cpu-fallback` and never touches `wgpu`; for `auto` (the
    // default since Issue #83) and `on` it triggers adapter selection now
    // so the same label is passed into every scoring path.
    // Issue #289: `resolve_mode` now returns the typed `GpuModeParseError`;
    // flatten it to the binary's `String` error contract at the boundary.
    let mode = gpu::resolve_mode(cli.gpu, std::env::var("NEAT_SCORER_GPU").ok().as_deref())
        .map_err(|e| e.to_string())?;

    // Issue #310: resolve the record-level sub-sampling spec once. `--sample-rate`
    // was range-validated by clap's value_parser; re-validate here so `--sample-phase`
    // is bound to the same spec (and fails loud on any inconsistency rather than
    // silently dropping the flag).
    let sample = match cli.sample_rate {
        Some(rate) => SampleSpec::new(rate, cli.sample_phase)?,
        None => SampleSpec::full(),
    };

    // Issue #121/#339/#316: `--gpu on` with a cost the kernels cannot serve is a
    // hard error. The batched/scratch kernels host MSE and RMSE (squared-error
    // sum, RMSE via a host-side `sqrt`) and MAE (absolute-error sum); every
    // other cost would be a wrong scoring result if silently downgraded. Check
    // this before adapter
    // selection so the error always mentions the unsupported cost, even on
    // machines without a GPU. `--gpu auto` (the default) falls back to CPU
    // instead; `--gpu off` never touches the GPU and is fine.
    if matches!(mode, GpuMode::On) && !cli.cost.gpu_supported() {
        return Err(format!(
            "GPU kernel not implemented for cost {}: the batched/scratch kernels host MSE, RMSE \
             and MAE only (use --gpu auto to silently fall back to CPU, or --gpu off to skip GPU detection)",
            cli.cost.as_str()
        ));
    }

    // Issue #180: under `--gpu auto` adapter selection is *deferred* to the
    // directory path, where a CPU-only pre-flight first checks that the GPU
    // kernel can host the creature set. Creating a `wgpu`/Metal device only to
    // abandon it (the set exceeds the 256-neuron shader cap) risked an abnormal
    // teardown that truncated stdout and surfaced to batch callers as
    // `exit 158` / `INVALID_JSON`. `on` still resolves up-front — the user
    // demanded a GPU, so a missing adapter must hard-error here as before.
    let (gpu_backend, gpu_ctx) = match mode {
        GpuMode::Off => (GpuBackendLabel::CpuFallback, None),
        GpuMode::Auto => (GpuBackendLabel::CpuFallback, None),
        GpuMode::On => match gpu::select_adapter() {
            Ok(Some(ctx)) => {
                let backend = ctx.backend;
                (backend, Some(Arc::new(ctx)))
            }
            Ok(None) => return Err(
                "No compatible GPU adapter found and --gpu on was requested (use --gpu auto to fall back to CPU, or --gpu off to skip GPU detection entirely)".to_string(),
            ),
            Err(e) => return Err(e.to_string()),
        },
    };

    // Issue #83 — codified ship/skip decision under Auto for the single-creature
    // path only; directory mode uses topology-aware [`auto_should_use_gpu_directory`].
    // `On` bypasses heuristics; `Off` skipped GPU detection above.

    if cli.creature_stdin {
        let (creature_json, data_path) = resolve_inputs(cli)?;
        // Issue #83: single-creature path stays on CPU under every mode
        // (#81 closed as a negative result — no GPU kernel ships for this
        // path). The reported `gpuBackend` reflects what actually ran, so
        // it is `cpu-fallback` here regardless of `gpu_backend` resolution.
        return score_from_json(
            &creature_json,
            &data_path,
            GpuBackendLabel::CpuFallback,
            cli.cost,
            &sample,
        )
        .map(|r| RunOutput::Single(Box::new(r)));
    }

    if cli.args.len() != 2 {
        return Err("Expected arguments: <creature.json|creatures_dir> <data_dir> (or use --creature-stdin)".to_string());
    }

    let creature_path = &cli.args[0];
    let data_path = &cli.args[1];
    if creature_path.is_dir() {
        // Issue #205: under `--gpu auto`, a non-MSE cost makes
        // `auto_should_use_gpu` return false, so the directory path runs on
        // CPU. That fallback was otherwise silent (only the
        // `gpuBackend: cpu-fallback` JSON field hinted at it). Emit one
        // informational stderr note naming the cost as the reason, mirroring
        // the other `[gpu] auto fallback ...` messages. No-op for MSE and for
        // explicit `--gpu on|off`.
        if let Some(note) =
            gpu::auto_cost_fallback_note(mode, ScoringPath::CreatureDirectory, cli.cost)
        {
            eprintln!("{note}");
        }
        // Issue #467: the topology probe loads and compiles every creature, so
        // run it once and share it between the fallback note and the routing
        // decision below. Only `auto` with a GPU-hosted cost consults it.
        let dir_probe = if matches!(mode, GpuMode::Auto) && cli.cost.gpu_supported() {
            multi_score::gpu_directory_probe_for_dir(creature_path.as_ref())
        } else {
            None
        };
        if let Some(note) = gpu::auto_topology_fallback_note(
            mode,
            ScoringPath::CreatureDirectory,
            cli.cost,
            dir_probe,
        ) {
            eprintln!("{note}");
        }
        // Directory mode: per Issue #82+#83 use the GPU multi-creature
        // batched kernel when (a) an adapter is available and (b) the mode
        // wants GPU for this path (`Auto` ⇒ topology-aware, `On` ⇒ yes, `Off` ⇒ no).
        // `inflight_chunks: 2` enables CPU↔GPU pipelining.
        let want_gpu_for_directory = match mode {
            GpuMode::Off => false,
            GpuMode::On => true,
            GpuMode::Auto => gpu::auto_should_use_gpu_directory(dir_probe, cli.cost),
        };
        if want_gpu_for_directory {
            // Resolve the GPU context for this directory. Under `--gpu on` it
            // was selected up-front. Under `--gpu auto` (Issue #180) selection
            // is deferred behind a CPU-only pre-flight: a creature set above
            // the 256-neuron shader cap routes straight to CPU *without* ever
            // creating a `wgpu`/Metal device, so there is no GPU context to
            // abort during teardown (the regression batch callers saw as
            // `exit 158` / `INVALID_JSON`).
            let resolved_ctx: Option<(GpuBackendLabel, Arc<gpu::GpuContext>)> = match mode {
                GpuMode::On => gpu_ctx.clone().map(|ctx| (gpu_backend, ctx)),
                GpuMode::Auto => match multi_score::gpu_directory_compatible(creature_path) {
                    // GPU-hostable — create the adapter now and run the kernel.
                    Ok(()) => match gpu::select_adapter() {
                        Ok(Some(ctx)) => {
                            let backend = ctx.backend;
                            Some((backend, Arc::new(ctx)))
                        }
                        // No adapter (or selection error) — `auto` must never
                        // abort scoring, so fall through to CPU silently.
                        _ => None,
                    },
                    // The set exceeds the shader cap (or uses an unsupported
                    // squash). Log the fallback and run on CPU — no device made.
                    Err(reason) => {
                        eprintln!(
                            "[gpu] auto fallback to CPU directory mode: GPU runner cannot host this creature set ({reason}); rerun with --gpu off"
                        );
                        None
                    }
                },
                // `want_gpu_for_path` is false under Off, so this is unreachable.
                GpuMode::Off => None,
            };

            if let Some((backend, ctx)) = resolved_ctx {
                match score_from_creature_dir_gpu_sampled(
                    creature_path,
                    data_path,
                    backend,
                    ctx,
                    2,
                    cli.cost,
                    &sample,
                ) {
                    Ok(r) => return Ok(RunOutput::Multi(r)),
                    Err(e) => {
                        // `--gpu on` is a hard requirement — surface the error.
                        // `--gpu auto` should never abort scoring: silently fall
                        // back to the CPU path so callers always get a result.
                        if matches!(mode, GpuMode::On) {
                            return Err(e);
                        }
                        eprintln!("[gpu] auto fallback to CPU directory mode: {e}");
                    }
                }
            }
        }
        // CPU directory mode — either Auto declined GPU (no adapter, kernel
        // could not host the creature set, or the path is not GPU-default),
        // or the mode is Off. Report `cpu-fallback` so `gpuBackend` reflects
        // what actually ran (Issue #83).
        score_from_creature_dir_sampled(
            creature_path,
            data_path,
            GpuBackendLabel::CpuFallback,
            cli.cost,
            &sample,
        )
        .map(RunOutput::Multi)
    } else {
        let creature_json = fs::read_to_string(creature_path).map_err(|e| {
            format!(
                "Failed to read creature file '{}': {e}",
                creature_path.display()
            )
        })?;
        // See note in the `--creature-stdin` branch — single-creature path
        // always reports `cpu-fallback`.
        score_from_json(
            &creature_json,
            data_path,
            GpuBackendLabel::CpuFallback,
            cli.cost,
            &sample,
        )
        .map(|r| RunOutput::Single(Box::new(r)))
    }
}

fn score_from_json(
    creature_json: &str,
    data_path: &Path,
    // Issue #470 vestigial-parameter sweep: always `CpuFallback` today because
    // Issue #81 closed single-creature GPU as a negative result. Kept because
    // it is read into `ScoreResult::gpu_backend` (the `gpuBackend` JSON field)
    // and keeps one reporting seam shared with the directory paths, so a future
    // single-creature kernel flips the label in one place.
    gpu_backend: GpuBackendLabel,
    // Issue #121 — resolved cost selector. Dispatched through the fused
    // streaming path for `forwardOnly` creatures and through a per-record
    // `accumulate_cost_sum` call for recurrent creatures.
    cost: CostKind,
    // Issue #310 — record-level sub-sampling spec; applies to both the fused
    // forward-only path and the per-record recurrent path so the flag never
    // silently no-ops for a single creature.
    sample: &SampleSpec,
) -> Result<ScoreResult, String> {
    let started = Instant::now();
    let creature = parse_creature_json(creature_json).map_err(|e| e.to_string())?;

    if creature.input == 0 || creature.output == 0 {
        return Err("Creature JSON must set positive input and output counts".to_string());
    }

    let compile_started = Instant::now();
    let mut network = compile_creature(&creature).map_err(|e| e.to_string())?;
    let mut compile_time_secs = compile_started.elapsed().as_secs_f64();
    let num_outputs = creature.output;

    if !data_path.is_dir() {
        return Err(format!(
            "Training data path '{}' is not a directory",
            data_path.display()
        ));
    }

    let bin_files = find_bin_files(data_path)
        .map_err(|e| format!("Failed to read training data directory: {e}"))?;
    if bin_files.is_empty() {
        return Err(format!(
            "No .bin files found in training data directory '{}'",
            data_path.display()
        ));
    }

    let config = TrainingDataConfig {
        num_inputs: creature.input,
        num_outputs: creature.output,
    };

    let record_bytes = config.bytes_per_record();
    let fused_read_target_bytes = training_read_target_bytes_from_env(record_bytes);
    let fused_read_buf_len =
        stream_score::effective_fused_read_buf_len(record_bytes, fused_read_target_bytes);

    let use_fused_stream = creature.forward_only;
    let activation_threads = use_fused_stream.then(activation_worker_count_for_scorer);
    let training_read_backend = if use_fused_stream {
        training_read_backend_label().to_string()
    } else {
        "record_iterator".to_string()
    };

    let (total_error, record_count, parallel_activation_batches, max_activation_batch_records) =
        if use_fused_stream {
            let (loss_sum, count, parallel_batches, max_batch, clone_secs) =
                stream_score::accumulate_cost_sum_forward_only_fused_sampled(
                    cost,
                    &bin_files,
                    &config,
                    &mut network,
                    *sample,
                )?;
            if count == 0 {
                return Err("No training records found".to_string());
            }
            // Per-worker clones run inside the fused accumulator; bundle them into compile time.
            compile_time_secs += clone_secs;
            (loss_sum, count, parallel_batches, max_batch)
        } else {
            // Recurrent / non-forward-only path. Issue #121: dispatch the
            // requested cost on a per-record packed buffer so every supported
            // CostKind works here too. The `forward_only = false` arg to
            // `accumulate_cost_sum` makes the underlying helper reset the
            // network state per record, matching the explicit `reset_state()`
            // the old MSE-only loop used to call.
            let mut iter = TrainingDataIterator::new(data_path, config.clone())
                .map_err(|e| format!("Failed to open training data iterator: {e}"))?;
            let mut total_error = 0.0_f64;
            let mut record_count: usize = 0;
            let mut packed: Vec<f32> = Vec::with_capacity(creature.input + num_outputs);
            // Issue #310: the same deterministic stride drops records here too so
            // `--sample-rate` is honoured (not silently ignored) for recurrent
            // creatures. The network state is reset per scored record below.
            let mut sampler = sample.sampler();

            while let Some(record) = iter
                .next_record()
                .map_err(|e| format!("Failed reading training record: {e}"))?
            {
                if !sampler.keep_next() {
                    continue;
                }
                packed.clear();
                packed.extend_from_slice(&record.inputs);
                packed.extend_from_slice(&record.outputs);
                total_error += cost::accumulate_cost_sum(
                    cost,
                    &mut network,
                    &packed,
                    creature.input,
                    num_outputs,
                    false,
                )?;
                record_count += 1;
            }

            if record_count == 0 {
                return Err("No training records found".to_string());
            }
            (total_error, record_count, 0_usize, 0_usize)
        };

    // Issue #339: route through the shared finaliser so RMSE gets its host-side
    // `sqrt` here, identically to the two directory-mode sites in `multi_score`.
    let avg_error = cost.finalise_mean(total_error, record_count);

    // Issue #289: the scoring API now returns the typed `ScoringError`; flatten
    // to the binary's `String` error contract at the boundary.
    let components = compute_score_components(&creature).map_err(|e| e.to_string())?;
    let hidden_neurons = components.hidden_neuron_count;
    let synapse_count = components.synapse_count;

    let complexity_penalty =
        scoring::complexity_penalty(&components, GROWTH_COST).map_err(|e| e.to_string())?;

    let score = calculate_score(
        avg_error,
        &components,
        GROWTH_COST,
        creature.semantic_version.as_deref(),
    )
    .map_err(|e| e.to_string())?;

    Ok(ScoreResult {
        score,
        error: avg_error,
        complexity_penalty,
        record_count,
        hidden_neurons,
        synapse_count,
        forward_only: creature.forward_only,
        training_read_backend,
        gpu_backend,
        read_buf_len: use_fused_stream.then_some(fused_read_buf_len),
        activation_threads: activation_threads.and_then(|n| (n > 1).then_some(n)),
        parallel_activation_batches: activation_threads
            .and_then(|n| (n > 1).then_some(parallel_activation_batches)),
        max_activation_batch_records: activation_threads
            .and_then(|n| (n > 1).then_some(max_activation_batch_records)),
        time_taken_secs: started.elapsed().as_secs_f64(),
        compile_time_secs: Some(compile_time_secs),
        gpu_kernel: None,
        gpu_inflight_chunks: None,
        gpu_dispatch_count: None,
        cost_name: cost.as_str().to_string(),
        // Issue #310: report the effective rate only when sub-sampling ran.
        sample_rate: (!sample.is_full()).then_some(sample.rate()),
    })
}

fn main() {
    let cli = Cli::parse();

    // Issue #201: route serialisation failures through the same
    // `eprintln!("Error: ...")` + `exit(1)` path as scoring errors, instead of
    // panicking via `expect`. `serde_json` errors on non-finite floats, so a
    // malformed result must exit cleanly rather than abort the process.
    let output = run(&cli).and_then(|out| match out {
        RunOutput::Single(result) => serde_json::to_string_pretty(&result)
            .map_err(|e| format!("Failed to serialise result to JSON: {e}")),
        RunOutput::Multi(result_map) => serde_json::to_string_pretty(&result_map)
            .map_err(|e| format!("Failed to serialise multi-creature result to JSON: {e}")),
    });

    match output {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run_single(cli: &Cli) -> Result<ScoreResult, String> {
        match run(cli)? {
            RunOutput::Single(result) => Ok(*result),
            RunOutput::Multi(_) => Err("Expected single-creature output".to_string()),
        }
    }

    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Helper: create a minimal creature JSON string. Defaults to
    /// `forwardOnly:true`, matching the feed-forward scoring path most tests
    /// exercise.
    fn make_creature_json(
        num_inputs: usize,
        num_outputs: usize,
        hidden_neurons: &[(&str, &str, f64)], // (uuid, squash, bias)
        synapses: &[(&str, &str, f64)],       // (from, to, weight)
        version: Option<&str>,
    ) -> String {
        make_creature_json_with_forward_only(
            num_inputs,
            num_outputs,
            hidden_neurons,
            synapses,
            version,
            true,
        )
    }

    /// Helper: create a minimal creature JSON string with an explicit
    /// `forwardOnly` flag. Issue #206: setting `forward_only=false` drives the
    /// recurrent single-creature branch of `score_from_json`, which uses the
    /// per-record `TrainingDataIterator` and reports the `record_iterator`
    /// read backend.
    fn make_creature_json_with_forward_only(
        num_inputs: usize,
        num_outputs: usize,
        hidden_neurons: &[(&str, &str, f64)], // (uuid, squash, bias)
        synapses: &[(&str, &str, f64)],       // (from, to, weight)
        version: Option<&str>,
        forward_only: bool,
    ) -> String {
        let mut neurons = Vec::new();

        for &(uuid, squash, bias) in hidden_neurons {
            neurons.push(format!(
                r#"{{"type":"hidden","uuid":"{uuid}","bias":{bias},"squash":"{squash}"}}"#
            ));
        }

        for i in 0..num_outputs {
            neurons.push(format!(
                r#"{{"type":"output","uuid":"output-{i}","bias":0.0,"squash":"IDENTITY"}}"#
            ));
        }

        let mut syn_strs = Vec::new();
        for &(from, to, weight) in synapses {
            syn_strs.push(format!(
                r#"{{"fromUUID":"{from}","toUUID":"{to}","weight":{weight}}}"#
            ));
        }

        let version_str = match version {
            Some(v) => format!(r#","semanticVersion":"{v}""#),
            None => String::new(),
        };

        format!(
            r#"{{"input":{num_inputs},"output":{num_outputs},"forwardOnly":{forward_only},"neurons":[{}],"synapses":[{}]{version_str}}}"#,
            neurons.join(","),
            syn_strs.join(","),
        )
    }

    /// Helper: write training records as binary files.
    fn write_training_data(
        dir: &std::path::Path,
        records: &[(Vec<f32>, Vec<f32>)], // (inputs, outputs)
    ) {
        let mut file = fs::File::create(dir.join("0.bin")).unwrap();
        for (inputs, outputs) in records {
            for &v in inputs.iter().chain(outputs.iter()) {
                file.write_all(&v.to_le_bytes()).unwrap();
            }
        }
    }

    /// Construct a default-mode `Cli` with the two positional args, matching the
    /// pre-issue-#15 contract (kept stable for these tests).
    fn cli_for(creature: &std::path::Path, data: &std::path::Path) -> Cli {
        Cli {
            creature_stdin: false,
            gpu: None,
            cost: CostKind::default(),
            sample_rate: None,
            sample_phase: 0,
            args: vec![creature.to_path_buf(), data.to_path_buf()],
        }
    }

    #[test]
    fn test_identity_network_zero_error() {
        // A simple network: input-0 -> output-0 with weight=1, bias=0
        // When given input=X, output should be X, so error against target=X is 0
        let tmp = TempDir::new().unwrap();
        let creature_path = tmp.path().join("creature.json");
        let data_dir = tmp.path().join("data");
        fs::create_dir(&data_dir).unwrap();

        let json = make_creature_json(1, 1, &[], &[("input-0", "output-0", 1.0)], Some("4.0.0"));
        fs::write(&creature_path, &json).unwrap();

        // Training data: input=0.5, expected output=0.5
        write_training_data(&data_dir, &[(vec![0.5], vec![0.5])]);

        let cli = cli_for(&creature_path, &data_dir);

        let result = run_single(&cli).unwrap();
        assert!(
            result.error.abs() < 1e-6,
            "Expected near-zero error, got {}",
            result.error
        );
        assert!(
            (result.score - 1.0).abs() < 1e-6,
            "Expected score near 1.0, got {}",
            result.score
        );
        assert_eq!(result.record_count, 1);
    }

    #[test]
    fn test_score_with_hidden_neuron() {
        let tmp = TempDir::new().unwrap();
        let creature_path = tmp.path().join("creature.json");
        let data_dir = tmp.path().join("data");
        fs::create_dir(&data_dir).unwrap();

        let json = make_creature_json(
            1,
            1,
            &[("hidden-0", "TANH", 0.0)],
            &[("input-0", "hidden-0", 1.0), ("hidden-0", "output-0", 1.0)],
            Some("4.0.0"),
        );
        fs::write(&creature_path, &json).unwrap();

        // Input=0 => tanh(0) = 0 => output = 0
        write_training_data(&data_dir, &[(vec![0.0], vec![0.0])]);

        let cli = cli_for(&creature_path, &data_dir);

        let result = run_single(&cli).unwrap();
        assert!(
            result.error.abs() < 1e-6,
            "Expected near-zero error, got {}",
            result.error
        );
        assert_eq!(result.hidden_neurons, 1);
        assert_eq!(result.synapse_count, 2);
        assert!(result.complexity_penalty > 0.0);
    }

    #[test]
    fn test_multiple_records() {
        let tmp = TempDir::new().unwrap();
        let creature_path = tmp.path().join("creature.json");
        let data_dir = tmp.path().join("data");
        fs::create_dir(&data_dir).unwrap();

        // Identity network: output = input
        let json = make_creature_json(1, 1, &[], &[("input-0", "output-0", 1.0)], Some("4.0.0"));
        fs::write(&creature_path, &json).unwrap();

        // Multiple records with some error
        write_training_data(
            &data_dir,
            &[
                (vec![1.0], vec![1.0]), // perfect
                (vec![0.0], vec![0.0]), // perfect
                (vec![0.5], vec![0.5]), // perfect
            ],
        );

        let cli = cli_for(&creature_path, &data_dir);

        let result = run_single(&cli).unwrap();
        assert_eq!(result.record_count, 3);
        assert!(result.error.abs() < 1e-6);
    }

    /// Issue #206: the recurrent (`forwardOnly:false`) single-creature branch
    /// of `score_from_json` must score correctly and report the
    /// `record_iterator` read backend. Every other test uses the default
    /// `forwardOnly:true` helper, so without this the `else` branch (distinct
    /// packed-buffer assembly + per-record state reset) had no coverage.
    #[test]
    fn test_recurrent_single_creature_uses_record_iterator_backend() {
        let tmp = TempDir::new().unwrap();
        let creature_path = tmp.path().join("creature.json");
        let data_dir = tmp.path().join("data");
        fs::create_dir(&data_dir).unwrap();

        // Identity network scored in recurrent mode (forwardOnly:false).
        let json = make_creature_json_with_forward_only(
            1,
            1,
            &[],
            &[("input-0", "output-0", 1.0)],
            Some("4.0.0"),
            false,
        );
        fs::write(&creature_path, &json).unwrap();
        write_training_data(&data_dir, &[(vec![1.0], vec![1.0]), (vec![0.5], vec![0.5])]);

        let result = run_single(&cli_for(&creature_path, &data_dir)).unwrap();

        // Numeric result: identity network has zero error and score ~1.0.
        assert!(
            result.error.abs() < 1e-6,
            "Expected near-zero error, got {}",
            result.error
        );
        assert!(
            (result.score - 1.0).abs() < 1e-6,
            "Expected score near 1.0, got {}",
            result.score
        );
        assert_eq!(result.record_count, 2);

        // Backend label and flag must reflect the recurrent branch.
        assert!(
            !result.forward_only,
            "recurrent creature must report forward_only=false"
        );
        assert_eq!(
            result.training_read_backend, "record_iterator",
            "recurrent branch must report the record_iterator backend, got {}",
            result.training_read_backend
        );
        // The fused-stream-only fields stay unset on the recurrent path.
        assert!(result.read_buf_len.is_none());
        assert!(result.activation_threads.is_none());
    }

    /// Issue #206: for a purely feed-forward (no recurrent synapses) network,
    /// the recurrent and forward-only paths reset state per record and so must
    /// produce the same numeric score — a parity sanity check across the two
    /// top-level single-creature scoring modes.
    #[test]
    fn test_recurrent_matches_forward_only_for_feed_forward_network() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        fs::create_dir(&data_dir).unwrap();
        write_training_data(
            &data_dir,
            &[(vec![0.25], vec![0.0]), (vec![0.75], vec![1.0])],
        );

        let hidden = [("hidden-0", "TANH", 0.1)];
        let synapses = [("input-0", "hidden-0", 1.3), ("hidden-0", "output-0", 0.9)];

        let forward_json =
            make_creature_json_with_forward_only(1, 1, &hidden, &synapses, Some("4.0.0"), true);
        let recurrent_json =
            make_creature_json_with_forward_only(1, 1, &hidden, &synapses, Some("4.0.0"), false);

        let forward = score_from_json(
            &forward_json,
            &data_dir,
            GpuBackendLabel::CpuFallback,
            CostKind::default(),
            &SampleSpec::full(),
        )
        .unwrap();
        let recurrent = score_from_json(
            &recurrent_json,
            &data_dir,
            GpuBackendLabel::CpuFallback,
            CostKind::default(),
            &SampleSpec::full(),
        )
        .unwrap();

        assert!(forward.forward_only);
        assert!(!recurrent.forward_only);
        assert_eq!(recurrent.training_read_backend, "record_iterator");

        // Same error and score across both modes for a feed-forward network.
        assert!(
            (forward.error - recurrent.error).abs() < 1e-9,
            "error mismatch: forward={} recurrent={}",
            forward.error,
            recurrent.error
        );
        assert!(
            (forward.score - recurrent.score).abs() < 1e-9,
            "score mismatch: forward={} recurrent={}",
            forward.score,
            recurrent.score
        );
        assert_eq!(forward.record_count, recurrent.record_count);
    }

    #[test]
    fn test_missing_creature_file() {
        let cli = cli_for(
            &PathBuf::from("/nonexistent/path/creature.json"),
            &PathBuf::from("/tmp"),
        );

        let result = run_single(&cli);
        assert!(result.is_err());
    }

    #[test]
    fn test_version_penalty_in_score() {
        let tmp = TempDir::new().unwrap();
        let creature_path_v4 = tmp.path().join("creature_v4.json");
        let creature_path_v3 = tmp.path().join("creature_v3.json");
        let data_dir = tmp.path().join("data");
        fs::create_dir(&data_dir).unwrap();

        let json_v4 = make_creature_json(1, 1, &[], &[("input-0", "output-0", 1.0)], Some("4.0.0"));
        let json_v3 = make_creature_json(1, 1, &[], &[("input-0", "output-0", 1.0)], Some("3.0.0"));
        fs::write(&creature_path_v4, &json_v4).unwrap();
        fs::write(&creature_path_v3, &json_v3).unwrap();
        write_training_data(&data_dir, &[(vec![0.5], vec![0.5])]);

        let cli_v4 = cli_for(&creature_path_v4, &data_dir);
        let cli_v3 = cli_for(&creature_path_v3, &data_dir);

        let result_v4 = run_single(&cli_v4).unwrap();
        let result_v3 = run_single(&cli_v3).unwrap();

        // v3 should have a 1e-6 version penalty
        assert!(
            (result_v4.score - result_v3.score - 1e-6).abs() < 1e-10,
            "Version penalty difference should be 1e-6, v4={}, v3={}",
            result_v4.score,
            result_v3.score
        );
    }

    #[test]
    fn test_empty_data_directory() {
        let tmp = TempDir::new().unwrap();
        let creature_path = tmp.path().join("creature.json");
        let data_dir = tmp.path().join("data");
        fs::create_dir(&data_dir).unwrap();

        let json = make_creature_json(1, 1, &[], &[("input-0", "output-0", 1.0)], None);
        fs::write(&creature_path, &json).unwrap();

        let cli = cli_for(&creature_path, &data_dir);

        let result = run_single(&cli);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No .bin files"));
    }

    #[test]
    fn test_json_output_format() {
        let result = ScoreResult {
            score: 0.85,
            error: 0.12,
            complexity_penalty: 0.03,
            record_count: 5000,
            hidden_neurons: 150,
            synapse_count: 2000,
            forward_only: true,
            training_read_backend: "native_pipelined".to_string(),
            gpu_backend: GpuBackendLabel::CpuFallback,
            read_buf_len: Some(2_097_152),
            activation_threads: Some(8),
            parallel_activation_batches: Some(1204),
            max_activation_batch_records: Some(2609),
            time_taken_secs: 1.25,
            compile_time_secs: Some(0.01),
            gpu_kernel: None,
            gpu_inflight_chunks: None,
            gpu_dispatch_count: None,
            cost_name: "MSE".to_string(),
            sample_rate: None,
        };
        let json = serde_json::to_string_pretty(&result).unwrap();
        // Verify camelCase keys in output
        assert!(json.contains("\"score\""));
        assert!(json.contains("\"error\""));
        assert!(json.contains("\"complexityPenalty\""));
        assert!(json.contains("\"recordCount\""));
        assert!(json.contains("\"hiddenNeurons\""));
        assert!(json.contains("\"synapseCount\""));
        assert!(json.contains("\"timeTaken\""));
        assert!(json.contains("\"forwardOnly\""));
        assert!(json.contains("\"trainingReadBackend\""));
        assert!(json.contains("\"gpuBackend\""));
        assert!(
            json.contains("\"cpu-fallback\""),
            "expected gpuBackend serialised as cpu-fallback, got: {json}"
        );
        assert!(json.contains("\"readBufLen\""));
        assert!(json.contains("\"activationThreads\""));
        assert!(json.contains("\"parallelActivationBatches\""));
        assert!(json.contains("\"maxActivationBatchRecords\""));
        assert!(json.contains("\"compileTimeSecs\""));
        // Issue #121: costName must be serialised so the TS bridge can
        // confirm the resolved cost.
        assert!(
            json.contains("\"costName\""),
            "expected costName in JSON, got: {json}"
        );
        assert!(json.contains("\"MSE\""), "expected costName value 'MSE'");
    }

    /// Issue #199: the serialised JSON `complexityPenalty` field must equal the
    /// penalty value baked into `score` by `calculate_score`. With a v4 creature
    /// the version penalty is zero, so `score == 1 - error - complexityPenalty`
    /// holds exactly when both use the one shared formula.
    #[test]
    fn test_complexity_penalty_json_matches_score() {
        let tmp = TempDir::new().unwrap();
        let creature_path = tmp.path().join("creature.json");
        let data_dir = tmp.path().join("data");
        fs::create_dir(&data_dir).unwrap();

        // Hidden neuron + weights > 1 so the complexity penalty is non-zero.
        let json = make_creature_json(
            1,
            1,
            &[("hidden-0", "TANH", 0.5)],
            &[("input-0", "hidden-0", 1.5), ("hidden-0", "output-0", 2.0)],
            Some("4.0.0"),
        );
        fs::write(&creature_path, &json).unwrap();
        write_training_data(&data_dir, &[(vec![0.5], vec![0.5])]);

        let result = run_single(&cli_for(&creature_path, &data_dir)).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
        let json_cp = value["complexityPenalty"]
            .as_f64()
            .expect("complexityPenalty must serialise as a number");

        assert!(json_cp > 0.0, "expected a non-zero penalty, got {json_cp}");
        assert!(
            (result.score - (1.0 - result.error - json_cp)).abs() < 1e-12,
            "JSON complexityPenalty {json_cp} disagrees with score {} (error {})",
            result.score,
            result.error
        );
    }

    /// Stdin mode must yield the same `ScoreResult` as the default file mode.
    /// Exercised via `score_from_json` — the same core the stdin path uses in
    /// `run`, bypassing the real `std::io::stdin` which cannot be injected in
    /// a unit test.
    #[test]
    fn test_stdin_mode_matches_file_mode() {
        let tmp = TempDir::new().unwrap();
        let creature_path = tmp.path().join("creature.json");
        let data_dir = tmp.path().join("data");
        fs::create_dir(&data_dir).unwrap();

        let json = make_creature_json(1, 1, &[], &[("input-0", "output-0", 1.0)], Some("4.0.0"));
        fs::write(&creature_path, &json).unwrap();
        write_training_data(&data_dir, &[(vec![0.5], vec![0.5])]);

        let file_result = run_single(&cli_for(&creature_path, &data_dir)).unwrap();
        let stdin_result = score_from_json(
            &json,
            &data_dir,
            GpuBackendLabel::CpuFallback,
            CostKind::default(),
            &SampleSpec::full(),
        )
        .unwrap();

        assert!((file_result.score - stdin_result.score).abs() < 1e-12);
        assert!((file_result.error - stdin_result.error).abs() < 1e-12);
        assert_eq!(file_result.record_count, stdin_result.record_count);
        assert_eq!(file_result.hidden_neurons, stdin_result.hidden_neurons);
        assert_eq!(file_result.synapse_count, stdin_result.synapse_count);
    }

    /// `--creature-stdin` with two positional args must be rejected before any
    /// stdin read (reading would block a unit test indefinitely).
    #[test]
    fn test_stdin_mode_rejects_extra_positional_args() {
        let cli = Cli {
            creature_stdin: true,
            gpu: None,
            cost: CostKind::default(),
            sample_rate: None,
            sample_phase: 0,
            args: vec![PathBuf::from("/tmp/creature.json"), PathBuf::from("/tmp")],
        };
        let err = resolve_inputs(&cli).expect_err("extra positional args should fail");
        assert!(
            err.contains("--creature-stdin"),
            "error should mention the flag, got: {err}"
        );
    }

    /// Default (file) mode must keep its two-positional contract.
    #[test]
    fn test_default_mode_requires_two_positional_args() {
        let cli = Cli {
            creature_stdin: false,
            gpu: None,
            cost: CostKind::default(),
            sample_rate: None,
            sample_phase: 0,
            args: vec![PathBuf::from("/tmp")],
        };
        let err = resolve_inputs(&cli).expect_err("single positional arg should fail");
        assert!(
            err.contains("<creature.json>") && err.contains("<data_dir>"),
            "error should describe the positional contract, got: {err}"
        );
    }

    /// `score_from_json` with an invalid JSON payload must error with a parse
    /// failure rather than panic — the stdin path reads arbitrary user input
    /// and must not crash the binary.
    #[test]
    fn test_score_from_json_rejects_invalid_json() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        fs::create_dir(&data_dir).unwrap();
        let err = score_from_json(
            "not json",
            &data_dir,
            GpuBackendLabel::CpuFallback,
            CostKind::default(),
            &SampleSpec::full(),
        )
        .expect_err("invalid JSON should fail");
        assert!(!err.is_empty());
    }

    /// Clap must accept `--creature-stdin` with a single positional arg and
    /// two positional args without the flag, both parsing cleanly.
    #[test]
    fn test_cli_parsing_both_modes() {
        use clap::Parser;

        let parsed = Cli::try_parse_from(["rust_scorer", "--creature-stdin", "/tmp/data"]).unwrap();
        assert!(parsed.creature_stdin);
        assert_eq!(parsed.args, vec![PathBuf::from("/tmp/data")]);

        let parsed =
            Cli::try_parse_from(["rust_scorer", "/tmp/creature.json", "/tmp/data"]).unwrap();
        assert!(!parsed.creature_stdin);
        assert_eq!(
            parsed.args,
            vec![
                PathBuf::from("/tmp/creature.json"),
                PathBuf::from("/tmp/data"),
            ]
        );
    }

    /// `--gpu` must accept `auto`, `on`, and `off`, and reject anything else
    /// at the clap layer (Issue #80).
    #[test]
    fn test_cli_parses_gpu_flag_values() {
        use clap::Parser;
        let parsed = Cli::try_parse_from([
            "rust_scorer",
            "--gpu",
            "off",
            "/tmp/creature.json",
            "/tmp/data",
        ])
        .unwrap();
        assert_eq!(parsed.gpu, Some(GpuMode::Off));

        let parsed = Cli::try_parse_from([
            "rust_scorer",
            "--gpu",
            "auto",
            "/tmp/creature.json",
            "/tmp/data",
        ])
        .unwrap();
        assert_eq!(parsed.gpu, Some(GpuMode::Auto));

        let parsed = Cli::try_parse_from([
            "rust_scorer",
            "--gpu",
            "on",
            "/tmp/creature.json",
            "/tmp/data",
        ])
        .unwrap();
        assert_eq!(parsed.gpu, Some(GpuMode::On));

        // No `--gpu` -> None (env var fallback handled by `gpu::resolve_mode`).
        let parsed =
            Cli::try_parse_from(["rust_scorer", "/tmp/creature.json", "/tmp/data"]).unwrap();
        assert_eq!(parsed.gpu, None);

        // Bogus value rejected.
        assert!(
            Cli::try_parse_from([
                "rust_scorer",
                "--gpu",
                "yolo",
                "/tmp/creature.json",
                "/tmp/data",
            ])
            .is_err()
        );
    }

    /// With `--gpu off`, `score_from_json` must report
    /// `gpu_backend = CpuFallback` in the result. This is the unit-level
    /// counterpart to the integration test in `tests/scorer_smoke.rs`.
    #[test]
    fn test_score_from_json_off_yields_cpu_fallback() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        fs::create_dir(&data_dir).unwrap();
        let json = make_creature_json(1, 1, &[], &[("input-0", "output-0", 1.0)], Some("4.0.0"));
        write_training_data(&data_dir, &[(vec![0.5], vec![0.5])]);

        let result = score_from_json(
            &json,
            &data_dir,
            GpuBackendLabel::CpuFallback,
            CostKind::default(),
            &SampleSpec::full(),
        )
        .unwrap();
        assert_eq!(result.gpu_backend, GpuBackendLabel::CpuFallback);
    }

    /// Issue #120: clap must accept every TS `BUILT_IN_COST_NAMES` value
    /// via `--cost` and store the corresponding `CostKind`.
    #[test]
    fn test_cli_parses_every_built_in_cost_name() {
        use clap::Parser;

        let cases = [
            ("MSE", CostKind::Mse),
            ("MAE", CostKind::Mae),
            ("MAPE", CostKind::Mape),
            ("MSLE", CostKind::Msle),
            ("HINGE", CostKind::Hinge),
            ("CROSS_ENTROPY", CostKind::CrossEntropy),
            ("CATEGORICAL_ERROR", CostKind::CategoricalError),
        ];
        for (name, expected) in cases {
            let parsed = Cli::try_parse_from([
                "rust_scorer",
                "--cost",
                name,
                "/tmp/creature.json",
                "/tmp/data",
            ])
            .unwrap_or_else(|e| panic!("clap rejected --cost {name}: {e}"));
            assert_eq!(parsed.cost, expected);
        }
    }

    /// Issue #339/#340: `--cost RMSE` must parse to `CostKind::Rmse`. RMSE is
    /// now a first-class upstream `BUILT_IN_COST_NAMES` value (synced under
    /// Issue #340, `stSoftwareAU/NEAT-AI#3341`); it is asserted separately from
    /// the "every built-in" contract test above because that test still tracks
    /// the historical seven-name clap contract.
    #[test]
    fn test_cli_parses_rmse() {
        use clap::Parser;
        let parsed = Cli::try_parse_from([
            "rust_scorer",
            "--cost",
            "RMSE",
            "/tmp/creature.json",
            "/tmp/data",
        ])
        .expect("clap must accept --cost RMSE");
        assert_eq!(parsed.cost, CostKind::Rmse);
    }

    /// Issue #339/#316: `--gpu on --cost {RMSE,MAE}` must clear the up-front
    /// guard that hard-errors non-GPU-supported costs. The guard at the top of
    /// `run` fires only when `!cli.cost.gpu_supported()`; MSE, RMSE and MAE are
    /// GPU-supported (RMSE reuses the MSE squared-error sum, MAE accumulates
    /// absolute error on the shared forward pass), so they clear the guard while
    /// the CPU-only costs still trip it.
    #[test]
    fn test_gpu_on_accepts_mse_rmse_and_mae() {
        assert!(CostKind::Mse.gpu_supported());
        assert!(CostKind::Rmse.gpu_supported());
        assert!(CostKind::Mae.gpu_supported());
        for cost in [
            CostKind::Mape,
            CostKind::Msle,
            CostKind::Hinge,
            CostKind::CrossEntropy,
            CostKind::CategoricalError,
        ] {
            assert!(
                !cost.gpu_supported(),
                "{} must still trip the --gpu on guard",
                cost.as_str()
            );
        }
    }

    /// Issue #120: omitting `--cost` must default to MSE so historical
    /// scoring behaviour is preserved.
    #[test]
    fn test_cli_default_cost_is_mse() {
        use clap::Parser;
        let parsed =
            Cli::try_parse_from(["rust_scorer", "/tmp/creature.json", "/tmp/data"]).unwrap();
        assert_eq!(parsed.cost, CostKind::Mse);
    }

    /// Issue #123: `--help` must enumerate every supported cost name so a
    /// user can discover the contract without leaving the CLI. Renders the
    /// long help (which includes `Possible values:`) and checks the full
    /// `BUILT_IN_COST_NAMES` set is present.
    #[test]
    fn test_help_enumerates_every_built_in_cost_name() {
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        let help = cmd.render_long_help().to_string();
        for name in [
            "MSE",
            "MAE",
            "MAPE",
            "MSLE",
            "HINGE",
            "CROSS_ENTROPY",
            "CATEGORICAL_ERROR",
        ] {
            assert!(
                help.contains(name),
                "rendered --help must enumerate cost '{name}', got:\n{help}"
            );
        }
        assert!(
            help.contains("--cost"),
            "rendered --help must mention --cost flag, got:\n{help}"
        );
    }

    /// Issue #123/#316: `--help` must note the GPU cost constraint so users
    /// picking a CPU-only cost understand they will run on the CPU pipeline.
    /// The GPU kernels host MSE, RMSE and MAE; the remaining costs fall back.
    #[test]
    fn test_help_notes_gpu_cost_constraint() {
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        let help = cmd.render_long_help().to_string();
        // The doc comment on the `--cost` flag must mention GPU support and the
        // hosted costs; phrasing kept loose so future kernel work can update the
        // wording without breaking the contract.
        let lower = help.to_lowercase();
        assert!(
            lower.contains("gpu")
                && lower.contains("mae")
                && lower.contains("rmse")
                && lower.contains("fall back"),
            "rendered --help must note the GPU cost support (MSE/RMSE/MAE + CPU fallback), got:\n{help}"
        );
    }

    /// Issue #123: `--help` must point users at the README so the
    /// long-form documentation (per-cost examples, dispatch behaviour) is
    /// discoverable from the CLI.
    #[test]
    fn test_help_links_to_readme() {
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        let help = cmd.render_long_help().to_string();
        assert!(
            help.contains("README"),
            "rendered --help must reference the README, got:\n{help}"
        );
    }

    /// Issue #120: unknown cost names must be rejected at the clap layer
    /// with a non-zero exit. The error message must mention the supported
    /// set so users can recover.
    #[test]
    fn test_cli_rejects_unknown_cost_name() {
        use clap::Parser;
        let err = Cli::try_parse_from([
            "rust_scorer",
            "--cost",
            "FOO",
            "/tmp/creature.json",
            "/tmp/data",
        ])
        .expect_err("unknown cost must be rejected");
        let rendered = err.to_string();
        // clap renders an error like "invalid value 'FOO' for '--cost <NAME>'
        // [possible values: MSE, MAE, ...]".
        assert!(
            rendered.contains("FOO"),
            "error must echo bad value, got: {rendered}"
        );
        assert!(
            rendered.contains("MSE") && rendered.contains("CROSS_ENTROPY"),
            "error must list supported costs, got: {rendered}"
        );
    }
}
