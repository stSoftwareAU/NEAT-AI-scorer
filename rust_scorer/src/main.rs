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

mod multi_score;
mod read_tuning;
mod scoring;
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

use crate::multi_score::score_from_creature_dir;
use crate::read_tuning::{training_read_backend_label, training_read_target_bytes_from_env};
use crate::scoring::{ScoreResult, calculate_score, compute_score_components};
use crate::stream_score::activation_worker_count_for_scorer;

/// Matches `DEFAULT_COST_OF_GROWTH` in `src/config/NeatConfig.ts` (CLI is KISS: no flag).
const GROWTH_COST: f64 = 0.000_000_1;

/// Full-dataset MSE score for a NEAT creature (native, minimal CLI).
///
/// Two input modes (positional contract unchanged):
/// * default: `rust_scorer <creature.json> <data_dir>`
/// * stdin:   `rust_scorer --creature-stdin <data_dir>` (creature JSON on stdin)
#[derive(Parser, Debug)]
#[command(
    name = "rust_scorer",
    about = "Full-dataset MSE fitness score for a NEAT creature (fast native path)",
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
    Single(ScoreResult),
    Multi(BTreeMap<String, ScoreResult>),
}

fn run(cli: &Cli) -> Result<RunOutput, String> {
    if cli.creature_stdin {
        let (creature_json, data_path) = resolve_inputs(cli)?;
        return score_from_json(&creature_json, &data_path).map(RunOutput::Single);
    }

    if cli.args.len() != 2 {
        return Err("Expected arguments: <creature.json|creatures_dir> <data_dir> (or use --creature-stdin)".to_string());
    }

    let creature_path = &cli.args[0];
    let data_path = &cli.args[1];
    if creature_path.is_dir() {
        score_from_creature_dir(creature_path, data_path).map(RunOutput::Multi)
    } else {
        let creature_json = fs::read_to_string(creature_path).map_err(|e| {
            format!(
                "Failed to read creature file '{}': {e}",
                creature_path.display()
            )
        })?;
        score_from_json(&creature_json, data_path).map(RunOutput::Single)
    }
}

fn score_from_json(creature_json: &str, data_path: &Path) -> Result<ScoreResult, String> {
    let started = Instant::now();
    let creature = parse_creature_json(creature_json)?;

    if creature.input == 0 || creature.output == 0 {
        return Err("Creature JSON must set positive input and output counts".to_string());
    }

    let compile_started = Instant::now();
    let mut network = compile_creature(&creature)?;
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
            let (mse_sum, count, parallel_batches, max_batch, clone_secs) =
                stream_score::accumulate_mse_sum_forward_only_fused(
                    &bin_files,
                    &config,
                    &creature,
                    &mut network,
                )?;
            if count == 0 {
                return Err("No training records found".to_string());
            }
            // Per-worker clones run inside the fused accumulator; bundle them into compile time.
            compile_time_secs += clone_secs;
            (mse_sum, count, parallel_batches, max_batch)
        } else {
            let mut iter = TrainingDataIterator::new(data_path, config.clone())
                .map_err(|e| format!("Failed to open training data iterator: {e}"))?;
            let mut total_error = 0.0_f64;
            let mut record_count: usize = 0;
            let mut output_buf = vec![0.0_f32; num_outputs];
            let inv_outputs = if num_outputs > 0 {
                1.0_f64 / num_outputs as f64
            } else {
                0.0
            };

            while let Some(record) = iter
                .next_record()
                .map_err(|e| format!("Failed reading training record: {e}"))?
            {
                network.reset_state();
                let outputs = network.activate(&record.inputs, num_outputs);
                output_buf.copy_from_slice(&outputs);
                // Per-record MSE = mean over outputs of (target - output)^2.
                // Computed inline to keep the recurrent loop independent of
                // `mse_mean_record`'s packed/batched signature in NEAT-AI-core.
                let mut sq_sum = 0.0_f64;
                for (target, predicted) in record.outputs.iter().zip(output_buf.iter()) {
                    let diff = (*target - *predicted) as f64;
                    sq_sum += diff * diff;
                }
                total_error += sq_sum * inv_outputs;
                record_count += 1;
            }

            if record_count == 0 {
                return Err("No training records found".to_string());
            }
            (total_error, record_count, 0_usize, 0_usize)
        };

    let avg_error = total_error / record_count as f64;

    let components = compute_score_components(&creature);
    let hidden_neurons = components.hidden_neuron_count;
    let synapse_count = components.synapse_count;

    let weight_bias_penalty = (scoring::value_penalty(components.max_weight_bias)
        + scoring::value_penalty(components.avg_weight_bias))
        / 2.0;
    let total_penalty = weight_bias_penalty + components.squash_complexity_penalty;
    let complexity_penalty = hidden_neurons as f64 * GROWTH_COST
        + synapse_count as f64 * GROWTH_COST / 10.0
        + total_penalty * GROWTH_COST / 100.0;

    let score = calculate_score(
        avg_error,
        &components,
        GROWTH_COST,
        creature.semantic_version.as_deref(),
    );

    Ok(ScoreResult {
        score,
        error: avg_error,
        complexity_penalty,
        record_count,
        hidden_neurons,
        synapse_count,
        forward_only: creature.forward_only,
        training_read_backend,
        read_buf_len: use_fused_stream.then_some(fused_read_buf_len),
        activation_threads: activation_threads.and_then(|n| (n > 1).then_some(n)),
        parallel_activation_batches: activation_threads
            .and_then(|n| (n > 1).then_some(parallel_activation_batches)),
        max_activation_batch_records: activation_threads
            .and_then(|n| (n > 1).then_some(max_activation_batch_records)),
        time_taken_secs: started.elapsed().as_secs_f64(),
        compile_time_secs: Some(compile_time_secs),
    })
}

fn main() {
    let cli = Cli::parse();

    match run(&cli) {
        Ok(RunOutput::Single(result)) => {
            let json =
                serde_json::to_string_pretty(&result).expect("Failed to serialise result to JSON");
            println!("{json}");
        }
        Ok(RunOutput::Multi(result_map)) => {
            let json = serde_json::to_string_pretty(&result_map)
                .expect("Failed to serialise multi-creature result to JSON");
            println!("{json}");
        }
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
            RunOutput::Single(result) => Ok(result),
            RunOutput::Multi(_) => Err("Expected single-creature output".to_string()),
        }
    }

    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Helper: create a minimal creature JSON string.
    fn make_creature_json(
        num_inputs: usize,
        num_outputs: usize,
        hidden_neurons: &[(&str, &str, f64)], // (uuid, squash, bias)
        synapses: &[(&str, &str, f64)],       // (from, to, weight)
        version: Option<&str>,
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
            r#"{{"input":{num_inputs},"output":{num_outputs},"forwardOnly":true,"neurons":[{}],"synapses":[{}]{version_str}}}"#,
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
            read_buf_len: Some(2_097_152),
            activation_threads: Some(8),
            parallel_activation_batches: Some(1204),
            max_activation_batch_records: Some(2609),
            time_taken_secs: 1.25,
            compile_time_secs: Some(0.01),
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
        assert!(json.contains("\"readBufLen\""));
        assert!(json.contains("\"activationThreads\""));
        assert!(json.contains("\"parallelActivationBatches\""));
        assert!(json.contains("\"maxActivationBatchRecords\""));
        assert!(json.contains("\"compileTimeSecs\""));
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
        let stdin_result = score_from_json(&json, &data_dir).unwrap();

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
        let err = score_from_json("not json", &data_dir).expect_err("invalid JSON should fail");
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
}
