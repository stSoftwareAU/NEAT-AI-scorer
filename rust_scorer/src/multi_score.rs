//! Multi-creature scoring with a single pass over training data.
//!
//! This path loads `*.json` creatures from a directory, validates they can be
//! scored together, then scans the training corpus exactly once and evaluates
//! all creatures in parallel across threads.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use neat_core::creature::{CreatureExport, compile_creature, parse_creature_json};
use neat_core::loss::mse_sum_batch_packed;
use neat_core::network::CompiledNetwork;
use neat_core::training_bin_stream::for_each_read_chunk;
use neat_core::training_data::{TrainingDataConfig, find_bin_files};
use rayon::prelude::*;

use crate::read_tuning::{training_read_backend_label, training_read_target_bytes_from_env};
use crate::scoring::{ScoreResult, calculate_score, compute_score_components, value_penalty};
use crate::stream_score::{activation_worker_count_for_scorer, effective_fused_read_buf_len};

/// Keep aligned with main scorer formula.
const GROWTH_COST: f64 = 0.000_000_1;
const PENDING_COMPACT_HEAD_BYTES: usize = 512 * 1024;

struct LoadedCreature {
    key: String,
    path: PathBuf,
    creature: CreatureExport,
    networks: Vec<CompiledNetwork>,
}

fn compact_pending_if_needed(pending: &mut Vec<u8>, head: &mut usize) {
    if *head == 0 {
        return;
    }
    let should_compact = *head >= PENDING_COMPACT_HEAD_BYTES || *head * 2 >= pending.len();
    if !should_compact {
        return;
    }
    let tail = pending.len() - *head;
    pending.copy_within(*head.., 0);
    pending.truncate(tail);
    *head = 0;
}

fn reserve_unpack_capacity(buf: &mut Vec<f32>, n: usize) {
    if buf.capacity() < n {
        buf.reserve(n - buf.capacity());
    }
}

fn unpack_f32s_le(src: &[u8], dst: &mut Vec<f32>, n: usize) {
    debug_assert_eq!(src.len(), n * 4);
    dst.clear();
    reserve_unpack_capacity(dst, n);

    #[cfg(target_endian = "little")]
    {
        // SAFETY: `src.len() == n * 4`, capacity ≥ `n` after `reserve_unpack_capacity`;
        // we initialise all `n` elements before `set_len(n)`.
        unsafe {
            let out_ptr = dst.as_mut_ptr();
            let p = src.as_ptr();
            for i in 0..n {
                let bits = p.add(i * 4).cast::<u32>().read_unaligned();
                out_ptr.add(i).write(f32::from_bits(bits));
            }
            dst.set_len(n);
        }
    }

    #[cfg(not(target_endian = "little"))]
    {
        for q in src.chunks_exact(4) {
            dst.push(f32::from_le_bytes([q[0], q[1], q[2], q[3]]));
        }
    }
}

fn partition_packed_records(
    records: &[f32],
    values_per_record: usize,
    n_records: usize,
    workers: usize,
) -> Vec<&[f32]> {
    debug_assert!(workers > 0 && workers <= n_records);
    debug_assert_eq!(records.len(), n_records * values_per_record);
    let base = n_records / workers;
    let rem = n_records % workers;
    let mut out = Vec::with_capacity(workers);
    let mut record_off = 0_usize;
    for w in 0..workers {
        let take = base + usize::from(w < rem);
        let start = record_off * values_per_record;
        let end = (record_off + take) * values_per_record;
        out.push(&records[start..end]);
        record_off += take;
    }
    out
}

fn load_creatures_from_dir(creatures_dir: &Path) -> Result<Vec<LoadedCreature>, String> {
    if !creatures_dir.is_dir() {
        return Err(format!(
            "Creature path '{}' is not a directory",
            creatures_dir.display()
        ));
    }

    let mut json_paths: Vec<PathBuf> = fs::read_dir(creatures_dir)
        .map_err(|e| {
            format!(
                "Failed to read creature directory '{}': {e}",
                creatures_dir.display()
            )
        })?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    json_paths.sort();

    if json_paths.is_empty() {
        return Err(format!(
            "No .json creature files found in directory '{}'",
            creatures_dir.display()
        ));
    }

    let mut loaded = Vec::with_capacity(json_paths.len());

    for path in json_paths {
        let creature_json = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read creature file '{}': {e}", path.display()))?;
        let key = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                format!(
                    "Creature file '{}' does not have a valid filename stem",
                    path.display()
                )
            })?
            .to_string();

        let creature = parse_creature_json(&creature_json)
            .map_err(|e| format!("Failed parsing creature '{}': {e}", path.display()))?;
        if creature.input == 0 || creature.output == 0 {
            return Err(format!(
                "Creature '{}' must set positive input and output counts",
                path.display()
            ));
        }
        if !creature.forward_only {
            return Err(format!(
                "Creature '{}' has forwardOnly=false; multi-creature directory mode requires forwardOnly=true for every creature",
                path.display()
            ));
        }

        let network = compile_creature(&creature)
            .map_err(|e| format!("Failed compiling creature '{}': {e}", path.display()))?;

        loaded.push(LoadedCreature {
            key,
            path,
            creature,
            networks: vec![network],
        });
    }

    let first = loaded
        .first()
        .expect("checked non-empty creature file list while loading");
    let expected_in = first.creature.input;
    let expected_out = first.creature.output;
    for c in loaded.iter().skip(1) {
        if c.creature.input != expected_in || c.creature.output != expected_out {
            return Err(format!(
                "Creature '{}' has input/output=({},{}) but expected ({},{}); all creatures in directory mode must share the same shape",
                c.path.display(),
                c.creature.input,
                c.creature.output,
                expected_in,
                expected_out
            ));
        }
    }

    Ok(loaded)
}

pub fn score_from_creature_dir(
    creatures_dir: &Path,
    data_path: &Path,
) -> Result<BTreeMap<String, ScoreResult>, String> {
    let started = Instant::now();
    let mut loaded = load_creatures_from_dir(creatures_dir)?;

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

    let num_inputs = loaded[0].creature.input;
    let num_outputs = loaded[0].creature.output;
    let config = TrainingDataConfig {
        num_inputs,
        num_outputs,
    };
    let record_bytes = config.bytes_per_record();
    if record_bytes == 0 {
        return Err("Invalid record byte length (zero)".to_string());
    }
    let values_per_record = num_inputs + num_outputs;

    let fused_read_target_bytes = training_read_target_bytes_from_env(record_bytes);
    let fused_read_buf_len = effective_fused_read_buf_len(record_bytes, fused_read_target_bytes);
    let training_read_backend = training_read_backend_label().to_string();
    let activation_threads = activation_worker_count_for_scorer();

    // Deeper parallelism: split each creature's chunk work across multiple worker
    // networks. Budget scales with CPU count and population size.
    let per_creature_workers = activation_threads.div_ceil(loaded.len()).max(1);
    if per_creature_workers > 1 {
        for loaded_creature in &mut loaded {
            loaded_creature.networks.reserve(per_creature_workers - 1);
            for _ in 1..per_creature_workers {
                loaded_creature.networks.push(
                    compile_creature(&loaded_creature.creature).map_err(|e| {
                        format!(
                            "Failed compiling worker network for creature '{}': {e}",
                            loaded_creature.path.display()
                        )
                    })?,
                );
            }
        }
    }

    let mut pending: Vec<u8> = Vec::new();
    let mut head: usize = 0;
    let mut unpack_floats: Vec<f32> = Vec::new();
    let mut total_records = 0_usize;
    let mut total_mse = vec![0.0_f64; loaded.len()];

    for_each_read_chunk(&bin_files, fused_read_buf_len, |chunk| {
        // Fast path: when nothing is buffered, score the aligned prefix of
        // `chunk` directly and only copy any trailing fragment into `pending`.
        // Avoids the `pending.extend_from_slice` memcpy on the common path
        // where the read buffer is a whole-record multiple. Issue #38.
        if pending.is_empty() && head == 0 && !chunk.is_empty() {
            let aligned_len = (chunk.len() / record_bytes) * record_bytes;
            if aligned_len > 0 {
                let num_f32 = aligned_len / 4;
                let n_records = aligned_len / record_bytes;
                unpack_f32s_le(&chunk[..aligned_len], &mut unpack_floats, num_f32);

                if unpack_floats.len() != n_records * values_per_record {
                    return Err("Internal float unpack length mismatch".to_string());
                }
                total_records += n_records;

                loaded
                    .par_iter_mut()
                    .zip(total_mse.par_iter_mut())
                    .for_each(|(loaded_creature, mse_sum)| {
                        let worker_count = loaded_creature.networks.len().min(n_records).max(1);
                        let chunk_sum = if worker_count > 1 {
                            let slices = partition_packed_records(
                                &unpack_floats,
                                values_per_record,
                                n_records,
                                worker_count,
                            );
                            loaded_creature.networks[..worker_count]
                                .par_iter_mut()
                                .zip(slices)
                                .map(|(net, slice)| {
                                    mse_sum_batch_packed(net, slice, num_inputs, num_outputs, true)
                                })
                                .sum()
                        } else {
                            mse_sum_batch_packed(
                                &mut loaded_creature.networks[0],
                                &unpack_floats,
                                num_inputs,
                                num_outputs,
                                true,
                            )
                        };
                        *mse_sum += chunk_sum;
                    });
            }
            if aligned_len < chunk.len() {
                pending.extend_from_slice(&chunk[aligned_len..]);
                // `head` stays at 0.
            }
            return Ok(());
        }

        // Slow path: residual bytes are buffered in `pending`; merge the new
        // chunk and consume whole records from the head.
        if !chunk.is_empty() {
            pending.extend_from_slice(chunk);
        }
        compact_pending_if_needed(&mut pending, &mut head);

        loop {
            let avail = pending.len() - head;
            let complete_len = (avail / record_bytes) * record_bytes;
            if complete_len == 0 {
                break;
            }

            let num_f32 = complete_len / 4;
            let n_records = complete_len / record_bytes;
            let slice = &pending[head..head + complete_len];
            unpack_f32s_le(slice, &mut unpack_floats, num_f32);

            if unpack_floats.len() != n_records * values_per_record {
                return Err("Internal float unpack length mismatch".to_string());
            }
            total_records += n_records;

            loaded
                .par_iter_mut()
                .zip(total_mse.par_iter_mut())
                .for_each(|(loaded_creature, mse_sum)| {
                    let worker_count = loaded_creature.networks.len().min(n_records).max(1);
                    let chunk_sum = if worker_count > 1 {
                        let slices = partition_packed_records(
                            &unpack_floats,
                            values_per_record,
                            n_records,
                            worker_count,
                        );
                        loaded_creature.networks[..worker_count]
                            .par_iter_mut()
                            .zip(slices)
                            .map(|(net, slice)| {
                                mse_sum_batch_packed(net, slice, num_inputs, num_outputs, true)
                            })
                            .sum()
                    } else {
                        mse_sum_batch_packed(
                            &mut loaded_creature.networks[0],
                            &unpack_floats,
                            num_inputs,
                            num_outputs,
                            true,
                        )
                    };
                    *mse_sum += chunk_sum;
                });

            head += complete_len;
        }

        // Drop fully-consumed `pending` so the next iteration can take the
        // fast path again.
        if head == pending.len() {
            pending.clear();
            head = 0;
        }

        Ok(())
    })?;

    if head != pending.len() {
        return Err(format!(
            "Trailing {} bytes (incomplete record) after reading all training files",
            pending.len() - head
        ));
    }
    if total_records == 0 {
        return Err("No training records found".to_string());
    }

    let elapsed = started.elapsed().as_secs_f64();
    let mut results = BTreeMap::new();
    for (loaded_creature, mse_sum) in loaded.iter().zip(total_mse.iter()) {
        let avg_error = *mse_sum / total_records as f64;
        let components = compute_score_components(&loaded_creature.creature);
        let hidden_neurons = components.hidden_neuron_count;
        let synapse_count = components.synapse_count;

        let weight_bias_penalty = (value_penalty(components.max_weight_bias)
            + value_penalty(components.avg_weight_bias))
            / 2.0;
        let total_penalty = weight_bias_penalty + components.squash_complexity_penalty;
        let complexity_penalty = hidden_neurons as f64 * GROWTH_COST
            + synapse_count as f64 * GROWTH_COST / 10.0
            + total_penalty * GROWTH_COST / 100.0;

        let score = calculate_score(
            avg_error,
            &components,
            GROWTH_COST,
            loaded_creature.creature.semantic_version.as_deref(),
        );

        results.insert(
            loaded_creature.key.clone(),
            ScoreResult {
                score,
                error: avg_error,
                complexity_penalty,
                record_count: total_records,
                hidden_neurons,
                synapse_count,
                forward_only: true,
                training_read_backend: training_read_backend.clone(),
                read_buf_len: Some(fused_read_buf_len),
                activation_threads: Some(activation_threads),
                parallel_activation_batches: None,
                max_activation_batch_records: None,
                time_taken_secs: elapsed,
            },
        );
    }

    Ok(results)
}
