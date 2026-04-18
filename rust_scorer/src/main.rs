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
mod scoring;
mod stream_score;

use std::fs;
use std::path::PathBuf;
use std::process;

use clap::Parser;
use neat_core::creature::{compile_creature, parse_creature_json};
use neat_core::training_data::{TrainingDataConfig, TrainingDataIterator, find_bin_files};

use crate::cost::mse_mean_record;
use crate::scoring::{ScoreResult, calculate_score, compute_score_components};

/// Matches `DEFAULT_COST_OF_GROWTH` in `src/config/NeatConfig.ts` (CLI is KISS: no flag).
const GROWTH_COST: f64 = 0.000_000_1;

/// Full-dataset MSE score for a NEAT creature (native, minimal CLI).
#[derive(Parser, Debug)]
#[command(
    name = "rust_scorer",
    about = "Full-dataset MSE fitness score for a NEAT creature (fast native path)",
    arg_required_else_help = true
)]
struct Cli {
    /// Path to the creature JSON export.
    creature: PathBuf,

    /// Directory of `.bin` training files (packed `f32`, little-endian).
    data: PathBuf,
}

fn run(cli: &Cli) -> Result<ScoreResult, String> {
    let creature_json = fs::read_to_string(&cli.creature).map_err(|e| {
        format!(
            "Failed to read creature file '{}': {e}",
            cli.creature.display()
        )
    })?;
    let creature = parse_creature_json(&creature_json)?;

    if creature.input == 0 || creature.output == 0 {
        return Err("Creature JSON must set positive input and output counts".to_string());
    }

    let mut network = compile_creature(&creature)?;
    let num_outputs = creature.output;

    let data_path = &cli.data;
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

    let use_fused_stream = creature.forward_only;

    let (total_error, record_count) = if use_fused_stream {
        let (mse_sum, count) =
            stream_score::accumulate_mse_sum_forward_only_fused(&bin_files, &config, &mut network)?;
        if count == 0 {
            return Err("No training records found".to_string());
        }
        (mse_sum, count)
    } else {
        let mut iter = TrainingDataIterator::new(data_path, config.clone())
            .map_err(|e| format!("Failed to open training data iterator: {e}"))?;
        let mut total_error = 0.0_f64;
        let mut record_count: usize = 0;
        let mut output_buf = vec![0.0_f32; num_outputs];

        while let Some(record) = iter
            .next_record()
            .map_err(|e| format!("Failed reading training record: {e}"))?
        {
            network.reset_state();
            let outputs = network.activate(&record.inputs, num_outputs);
            output_buf.copy_from_slice(&outputs);
            let record_error = mse_mean_record(&record.outputs, &output_buf);
            total_error += record_error;
            record_count += 1;
        }

        if record_count == 0 {
            return Err("No training records found".to_string());
        }
        (total_error, record_count)
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
    })
}

fn main() {
    let cli = Cli::parse();

    match run(&cli) {
        Ok(result) => {
            let json =
                serde_json::to_string_pretty(&result).expect("Failed to serialise result to JSON");
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

        let cli = Cli {
            creature: creature_path.clone(),
            data: data_dir.clone(),
        };

        let result = run(&cli).unwrap();
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

        let cli = Cli {
            creature: creature_path.clone(),
            data: data_dir.clone(),
        };

        let result = run(&cli).unwrap();
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

        let cli = Cli {
            creature: creature_path.clone(),
            data: data_dir.clone(),
        };

        let result = run(&cli).unwrap();
        assert_eq!(result.record_count, 3);
        assert!(result.error.abs() < 1e-6);
    }

    #[test]
    fn test_missing_creature_file() {
        let cli = Cli {
            creature: PathBuf::from("/nonexistent/path/creature.json"),
            data: PathBuf::from("/tmp"),
        };

        let result = run(&cli);
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

        let cli_v4 = Cli {
            creature: creature_path_v4.clone(),
            data: data_dir.clone(),
        };
        let cli_v3 = Cli {
            creature: creature_path_v3.clone(),
            data: data_dir.clone(),
        };

        let result_v4 = run(&cli_v4).unwrap();
        let result_v3 = run(&cli_v3).unwrap();

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

        let cli = Cli {
            creature: creature_path.clone(),
            data: data_dir.clone(),
        };

        let result = run(&cli);
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
        };
        let json = serde_json::to_string_pretty(&result).unwrap();
        // Verify camelCase keys in output
        assert!(json.contains("\"score\""));
        assert!(json.contains("\"error\""));
        assert!(json.contains("\"complexityPenalty\""));
        assert!(json.contains("\"recordCount\""));
        assert!(json.contains("\"hiddenNeurons\""));
        assert!(json.contains("\"synapseCount\""));
    }
}
