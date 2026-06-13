//! Multi-creature scoring with a single pass over training data.
//!
//! This path loads `*.json` creatures from a directory, validates they can be
//! scored together, then scans the training corpus exactly once and evaluates
//! all creatures in parallel across threads.
//!
//! ## Parallelism (Issue #41)
//!
//! Each chunk of training records is scored through a single flat `par_iter`
//! over a precomputed pool of worker networks, sized so total parallelism
//! equals `activation_threads`. There is exactly one Rayon parallel layer in
//! the per-chunk hot loop:
//!
//! - When `loaded.len() >= activation_threads`, every creature owns one
//!   worker network and the par_iter has `loaded.len()` items.
//! - When `loaded.len() < activation_threads`, the `activation_threads` budget
//!   is distributed across creatures (`base + 1` workers for the first
//!   `rem` creatures), and each chunk's records are partitioned into one
//!   slice per worker so the par_iter still has `activation_threads` items.
//!
//! Earlier revisions ran an outer `par_iter_mut` over creatures and an inner
//! `par_iter_mut` over per-creature worker networks, which oversubscribed the
//! global Rayon pool with `creatures × workers` tasks.

use std::collections::BTreeMap;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use neat_core::creature::{CreatureExport, compile_creature, parse_creature_json};
use neat_core::network::CompiledNetwork;
use neat_core::training_bin_stream::for_each_read_chunk;
use neat_core::training_data::{TrainingDataConfig, find_bin_files};
use rayon::prelude::*;

use crate::cost::{CostKind, accumulate_cost_sum};
use crate::gpu::forward_mse_batched::{BatchedRunner, build_batched_network_data};
use crate::gpu::{GpuBackendLabel, GpuContext};
use crate::read_tuning::{training_read_backend_label, training_read_target_bytes_from_env};
use crate::scoring::{ScoreResult, calculate_score, complexity_penalty, compute_score_components};
use crate::stream_score::{activation_worker_count_for_scorer, effective_fused_read_buf_len};

/// Keep aligned with main scorer formula.
const GROWTH_COST: f64 = 0.000_000_1;
const PENDING_COMPACT_HEAD_BYTES: usize = 512 * 1024;

struct LoadedCreature {
    key: String,
    path: PathBuf,
    creature: CreatureExport,
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

/// Decode little-endian `f32` bytes.
///
/// # Panics
/// Panics if `src.len() != n * 4`. This runtime check runs in both debug and
/// release builds because the `little` branch performs raw pointer arithmetic
/// that relies on the length invariant — a malformed `.bin` chunk must not be
/// allowed to drive an out-of-bounds read (Issue #103).
fn unpack_f32s_le(src: &[u8], dst: &mut Vec<f32>, n: usize) {
    assert_eq!(
        src.len(),
        n * 4,
        "unpack_f32s_le: src.len() ({}) != n * 4 ({})",
        src.len(),
        n * 4
    );
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

/// Partition `n_records` packed records into `workers` ranges over the
/// flat float buffer. Each returned range is a half-open `[start, end)` over
/// the f32 slice. `workers` must be in `[1, n_records]`.
fn partition_packed_record_ranges(
    values_per_record: usize,
    n_records: usize,
    workers: usize,
) -> Vec<Range<usize>> {
    debug_assert!(workers > 0 && workers <= n_records);
    let base = n_records / workers;
    let rem = n_records % workers;
    let mut out = Vec::with_capacity(workers);
    let mut record_off = 0_usize;
    for w in 0..workers {
        let take = base + usize::from(w < rem);
        let start = record_off * values_per_record;
        let end = (record_off + take) * values_per_record;
        out.push(start..end);
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

        loaded.push(LoadedCreature {
            key,
            path,
            creature,
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

/// Compute per-creature worker counts so total parallelism is approximately
/// `activation_threads`. When `loaded.len() >= activation_threads` each
/// creature gets exactly one worker (the inner record split is dropped); when
/// `loaded.len() < activation_threads` the budget is spread across creatures
/// so total worker count equals `activation_threads`.
fn workers_per_creature(n_creatures: usize, activation_threads: usize) -> Vec<usize> {
    debug_assert!(n_creatures > 0);
    let threads = activation_threads.max(1);
    if n_creatures >= threads {
        return vec![1; n_creatures];
    }
    let base = threads / n_creatures;
    let rem = threads % n_creatures;
    (0..n_creatures)
        .map(|i| (base + usize::from(i < rem)).max(1))
        .collect()
}

/// Score every creature in `creatures_dir` against the `.bin` training records
/// under `data_path`, returning per-creature [`ScoreResult`]s keyed by creature
/// id.
///
/// `gpu_backend` selects the scoring pipeline (CPU fallback vs. `wgpu`), and
/// `cost` is the resolved loss function dispatched through
/// [`accumulate_cost_sum`] inside the per-chunk hot loop. Returns `Err` with a
/// human-readable message on I/O, shape, or cost-resolution failure.
pub fn score_from_creature_dir(
    creatures_dir: &Path,
    data_path: &Path,
    gpu_backend: GpuBackendLabel,
    // Issue #121 — resolved cost selector; dispatched through
    // [`accumulate_cost_sum`] inside the per-chunk hot loop.
    cost: CostKind,
) -> Result<BTreeMap<String, ScoreResult>, String> {
    let started = Instant::now();
    let loaded = load_creatures_from_dir(creatures_dir)?;

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

    // Issue #41: build a flat pool of worker networks sized to
    // `activation_threads`. Each chunk runs a single `par_iter_mut` over this
    // pool — no nested Rayon — so the global thread pool sees at most
    // `activation_threads` concurrent activation tasks regardless of
    // population size.
    let workers_per = workers_per_creature(loaded.len(), activation_threads);
    let total_workers: usize = workers_per.iter().sum();

    // Prefix-sum offsets: worker i for creature c lives at
    // `flat_networks[worker_offsets[c] + i]`.
    let mut worker_offsets: Vec<usize> = Vec::with_capacity(loaded.len() + 1);
    let mut acc = 0usize;
    worker_offsets.push(0);
    for &w in &workers_per {
        acc += w;
        worker_offsets.push(acc);
    }

    // Reverse map: each worker → its owning creature index.
    let mut worker_creature_idx: Vec<usize> = Vec::with_capacity(total_workers);
    for (ci, &w) in workers_per.iter().enumerate() {
        for _ in 0..w {
            worker_creature_idx.push(ci);
        }
    }

    // Issue #42: compile each creature exactly once; clone the resulting
    // `CompiledNetwork` for any additional workers. `CompiledNetwork: Clone`
    // landed upstream (NEAT-AI-core#11), so a clone is functionally equivalent
    // to a second `compile_creature` call but skips JSON-graph traversal,
    // UUID resolution, and topological ordering. For 50 creatures × 16
    // workers this collapses 800 compiles into 50.
    let compile_started = Instant::now();
    let mut flat_networks: Vec<CompiledNetwork> = Vec::with_capacity(total_workers);
    for (ci, c) in loaded.iter().enumerate() {
        let template = compile_creature(&c.creature).map_err(|e| {
            format!(
                "Failed compiling worker network for creature '{}': {e}",
                c.path.display()
            )
        })?;
        let workers = workers_per[ci];
        for _ in 0..workers.saturating_sub(1) {
            flat_networks.push(template.clone());
        }
        flat_networks.push(template);
    }
    let compile_time_secs = compile_started.elapsed().as_secs_f64();

    // Issue #121: validate that the requested cost is dispatchable before
    // entering the I/O loop. The probe uses an empty chunk — all supported
    // costs short-circuit to 0.0 on zero records. Every built-in cost
    // dispatches today (Issue #134 wired the last one, `CATEGORICAL_ERROR`);
    // the probe stays so a future kernel-only cost that surfaces an error
    // here is caught before any bytes are read.
    if !flat_networks.is_empty() {
        accumulate_cost_sum(
            cost,
            &mut flat_networks[0],
            &[],
            num_inputs,
            num_outputs,
            true,
        )?;
    }

    let mut pending: Vec<u8> = Vec::new();
    let mut head: usize = 0;
    let mut unpack_floats: Vec<f32> = Vec::new();
    let mut total_records = 0_usize;
    let mut total_mse = vec![0.0_f64; loaded.len()];
    // Reused per-chunk buffers — populated inside `score_chunk`.
    let mut work_ranges: Vec<Option<Range<usize>>> = vec![None; total_workers];
    let mut worker_sums: Vec<f64> = vec![0.0; total_workers];

    let mut score_chunk = |floats: &[f32], n_records: usize| -> Result<(), String> {
        if floats.len() != n_records * values_per_record {
            return Err("Internal float unpack length mismatch".to_string());
        }
        total_records += n_records;

        // Reset per-chunk scratch: ranges default to "no work" so workers
        // whose creature has fewer records than nominal worker count idle.
        for slot in work_ranges.iter_mut() {
            *slot = None;
        }

        // Fill work_ranges per creature.
        for (ci, &nominal_w) in workers_per.iter().enumerate() {
            let off = worker_offsets[ci];
            let actual_w = nominal_w.min(n_records).max(1);
            if actual_w == 1 {
                work_ranges[off] = Some(0..floats.len());
            } else {
                let ranges = partition_packed_record_ranges(values_per_record, n_records, actual_w);
                for (j, r) in ranges.into_iter().enumerate() {
                    work_ranges[off + j] = Some(r);
                }
            }
        }

        // Single flat par_iter over the worker pool — this is the only
        // Rayon parallel layer in the per-chunk hot path (Issue #41).
        // Issue #121: dispatch on the resolved cost; cost was validated
        // up-front (see the empty-chunk probe before the I/O loop).
        // Issue #200: propagate any per-chunk cost error out of the parallel
        // closure via `try_for_each` instead of panicking with `.expect`, so
        // a content-dependent failure mode returns a clean `Err(String)`
        // rather than aborting the whole binary across a Rayon worker.
        flat_networks
            .par_iter_mut()
            .zip(worker_sums.par_iter_mut())
            .enumerate()
            .try_for_each(|(worker_idx, (net, sum))| -> Result<(), String> {
                if let Some(range) = &work_ranges[worker_idx] {
                    *sum = accumulate_cost_sum(
                        cost,
                        net,
                        &floats[range.clone()],
                        num_inputs,
                        num_outputs,
                        true,
                    )?;
                } else {
                    *sum = 0.0;
                }
                Ok(())
            })?;

        // Reduce per-worker sums into per-creature totals (sequential —
        // total_workers ≤ activation_threads, so this is cheap).
        for (worker_idx, &s) in worker_sums.iter().enumerate() {
            total_mse[worker_creature_idx[worker_idx]] += s;
        }

        Ok(())
    };

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
                score_chunk(&unpack_floats, n_records)?;
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
            score_chunk(&unpack_floats, n_records)?;

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
        let components = compute_score_components(&loaded_creature.creature)?;
        let hidden_neurons = components.hidden_neuron_count;
        let synapse_count = components.synapse_count;

        let complexity_penalty = complexity_penalty(&components, GROWTH_COST)?;

        let score = calculate_score(
            avg_error,
            &components,
            GROWTH_COST,
            loaded_creature.creature.semantic_version.as_deref(),
        )?;

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
                gpu_backend,
                read_buf_len: Some(fused_read_buf_len),
                activation_threads: Some(activation_threads),
                parallel_activation_batches: None,
                max_activation_batch_records: None,
                time_taken_secs: elapsed,
                compile_time_secs: Some(compile_time_secs),
                gpu_kernel: None,
                gpu_inflight_chunks: None,
                gpu_dispatch_count: None,
                cost_name: cost.as_str().to_string(),
            },
        );
    }

    Ok(results)
}

/// Issue #180 — CPU-only pre-flight that decides whether a GPU kernel can host
/// every creature in `creatures_dir` **without creating a `wgpu` device**.
///
/// `main.rs` calls this under `--gpu auto` *before* selecting a GPU adapter: a
/// created-then-abandoned Metal context could abort during process teardown —
/// truncating the JSON the batch caller reads and surfacing as `exit 158` /
/// `INVALID_JSON`. Gating adapter creation on this cheap CPU check means an
/// unhostable set never spins up — and never tears down — a GPU context, so the
/// CPU fallback returns valid JSON and exits cleanly.
///
/// Issue #182 lifted the 256-neuron cap: creatures of any realistic size
/// (observed: 4139 neurons) are now GPU-hostable via the `forward_mse_scratch`
/// kernel, so a large creature set no longer forces a CPU fallback here.
///
/// Returns:
/// * `Err(reason)` — the set is genuinely incompatible with the GPU kernels (an
///   unsupported squash, a shape mismatch, or an absurd neuron count beyond
///   [`crate::gpu::forward_mse_batched::MAX_NEURONS_ABSOLUTE`]). `reason` is the
///   human-readable cause.
/// * `Ok(())` — either the set is GPU-hostable, or it could not even be
///   loaded/compiled. Load/compile failures are deliberately *not* treated as
///   GPU-incompatibility: they are left for the normal scoring path to surface
///   with its existing, precise error message (e.g. shape mismatch,
///   `forwardOnly=false`).
pub fn gpu_directory_compatible(creatures_dir: &Path) -> Result<(), String> {
    // A load failure here (missing dir, shape mismatch, forwardOnly=false, …)
    // is not a GPU-hostability question — return Ok so the real scoring path
    // re-runs the load and reports the precise error itself.
    let loaded = match load_creatures_from_dir(creatures_dir) {
        Ok(loaded) if !loaded.is_empty() => loaded,
        _ => return Ok(()),
    };

    let num_inputs = loaded[0].creature.input;
    let num_outputs = loaded[0].creature.output;

    let mut networks: Vec<CompiledNetwork> = Vec::with_capacity(loaded.len());
    for c in &loaded {
        match compile_creature(&c.creature) {
            Ok(net) => networks.push(net),
            // Compile failures are surfaced by the scoring path, not here.
            Err(_) => return Ok(()),
        }
    }

    build_batched_network_data(&networks, num_inputs, num_outputs)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Issue #82 — GPU-backed multi-creature scoring path.
///
/// Same I/O envelope as [`score_from_creature_dir`]: load every `*.json`
/// creature from `creatures_dir`, validate the shared `(num_inputs,
/// num_outputs)` shape, then stream the training corpus once and accumulate
/// per-creature MSE. The per-chunk inner loop dispatches a single
/// `forward_mse_batched` GPU kernel that scores every (creature, record) pair
/// in the chunk; per-creature partials come back from the GPU summed over
/// records and are added to the running per-creature totals on the host.
///
/// `inflight_chunks` caps in-flight GPU dispatches:
/// * `1` — synchronous; the I/O thread waits for each chunk's readback
///   before reading the next one.
/// * `2` — pipelined; chunk `N+1`'s host unpack overlaps chunk `N`'s GPU
///   compute via a worker thread fed by a bounded channel. Higher values are
///   accepted but capped to `2` so device memory stays bounded.
///
/// Returns the same per-creature `BTreeMap<String, ScoreResult>` as the CPU
/// path, with `gpuKernel` (`forward_mse_batched` for creatures within the 256
/// cap, `forward_mse_scratch` above it — Issue #182), `gpuInflightChunks`, and
/// `gpuDispatchCount` populated for every entry.
pub fn score_from_creature_dir_gpu(
    creatures_dir: &Path,
    data_path: &Path,
    gpu_backend: GpuBackendLabel,
    ctx: Arc<GpuContext>,
    inflight_chunks: usize,
    // Issue #121 — the GPU `forward_mse_batched` kernel only computes MSE.
    // Any other cost is a hard error here; callers under `--gpu auto` are
    // expected to detect this via [`crate::gpu::auto_should_use_gpu`] and
    // route to the CPU path instead.
    cost: CostKind,
) -> Result<BTreeMap<String, ScoreResult>, String> {
    if !cost.gpu_supported() {
        return Err(format!(
            "GPU kernel not implemented for cost {}: forward_mse_batched only handles MSE \
             (use --gpu auto for silent CPU fallback, or --gpu off to skip GPU entirely)",
            cost.as_str()
        ));
    }
    let started = Instant::now();
    let loaded = load_creatures_from_dir(creatures_dir)?;

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

    // Compile every creature exactly once — the GPU runner serialises the
    // resulting topology into flat per-creature SSBOs.
    let compile_started = Instant::now();
    let mut networks: Vec<CompiledNetwork> = Vec::with_capacity(loaded.len());
    for c in &loaded {
        let net = compile_creature(&c.creature).map_err(|e| {
            format!(
                "Failed compiling network for creature '{}': {e}",
                c.path.display()
            )
        })?;
        networks.push(net);
    }
    let compile_time_secs = compile_started.elapsed().as_secs_f64();

    // Build the GPU runner up-front; if any creature is incompatible with the
    // shader (unsupported squash, too many neurons, shape mismatch) we fall
    // back to the CPU path so callers still get a result.
    let mut runner = match BatchedRunner::new(ctx.clone(), &networks, num_inputs, num_outputs) {
        Ok(r) => r,
        Err(e) => {
            return Err(format!(
                "GPU runner cannot host this creature set ({e}); rerun with --gpu off"
            ));
        }
    };

    let inflight_chunks = inflight_chunks.clamp(1, 2);
    let mut total_records = 0_usize;
    let mut total_mse = vec![0.0_f64; loaded.len()];
    let mut pending: Vec<u8> = Vec::new();
    let mut head: usize = 0;
    let mut unpack_floats: Vec<f32> = Vec::new();

    if inflight_chunks <= 1 {
        // Synchronous: one chunk in flight at a time.
        let mut score_chunk = |floats: &[f32], n_records: usize| -> Result<(), String> {
            if floats.len() != n_records * values_per_record {
                return Err("Internal float unpack length mismatch".to_string());
            }
            total_records += n_records;
            let sums = runner.score_chunk(floats, n_records);
            for (i, s) in sums.iter().enumerate() {
                total_mse[i] += s;
            }
            Ok(())
        };

        for_each_read_chunk(&bin_files, fused_read_buf_len, |chunk| {
            run_io_loop(
                chunk,
                &mut pending,
                &mut head,
                &mut unpack_floats,
                record_bytes,
                &mut score_chunk,
            )
        })?;
    } else {
        // Pipelined: a worker thread runs GPU dispatches while the I/O thread
        // continues unpacking. Bounded crossbeam-style channel of capacity
        // `inflight_chunks` keeps memory bounded under streaming load.
        use std::sync::mpsc;
        use std::thread;

        // Move runner into worker thread; results come back through `result_rx`.
        let n_creatures = loaded.len();
        let (work_tx, work_rx) = mpsc::sync_channel::<Option<(Vec<f32>, usize)>>(inflight_chunks);
        let (result_tx, result_rx) = mpsc::channel::<Result<(usize, Vec<f64>), String>>();

        let worker = thread::spawn(move || {
            while let Ok(Some((floats, n_records))) = work_rx.recv() {
                if floats.len() != n_records * values_per_record {
                    let _ =
                        result_tx.send(Err("Internal float unpack length mismatch".to_string()));
                    return runner;
                }
                let sums = runner.score_chunk(&floats, n_records);
                if result_tx.send(Ok((n_records, sums))).is_err() {
                    break;
                }
            }
            runner
        });

        let mut pending_results: usize = 0;
        let drain_one = |total_records: &mut usize,
                         total_mse: &mut [f64],
                         pending_results: &mut usize|
         -> Result<(), String> {
            match result_rx.recv() {
                Ok(Ok((n_records, sums))) => {
                    *total_records += n_records;
                    for (i, s) in sums.iter().enumerate() {
                        total_mse[i] += s;
                    }
                    *pending_results -= 1;
                    Ok(())
                }
                Ok(Err(e)) => Err(e),
                Err(_) => Err("GPU worker hung up unexpectedly".to_string()),
            }
        };

        let mut submit_chunk = |floats: &[f32], n_records: usize| -> Result<(), String> {
            // Cap in-flight so the channel never queues more than `inflight_chunks`
            // chunks. When already at the cap, drain one result first.
            while pending_results >= inflight_chunks {
                drain_one(&mut total_records, &mut total_mse, &mut pending_results)?;
            }
            // Send a copy — the worker thread takes ownership of the Vec.
            work_tx
                .send(Some((floats.to_vec(), n_records)))
                .map_err(|_| "GPU worker hung up before chunk submission".to_string())?;
            pending_results += 1;
            Ok(())
        };

        for_each_read_chunk(&bin_files, fused_read_buf_len, |chunk| {
            run_io_loop(
                chunk,
                &mut pending,
                &mut head,
                &mut unpack_floats,
                record_bytes,
                &mut submit_chunk,
            )
        })?;

        // Drain remaining results before tearing down the worker.
        while pending_results > 0 {
            drain_one(&mut total_records, &mut total_mse, &mut pending_results)?;
        }
        let _ = work_tx.send(None);
        let recovered_runner = worker.join().map_err(|_| "GPU worker thread panicked")?;
        // Move dispatch_count back so we can report it in the JSON.
        runner = recovered_runner;
        // Quiet "unused" warnings in release builds.
        let _ = n_creatures;
    }

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
    let dispatch_count = runner.dispatch_count;
    // Report the kernel that actually ran — `forward_mse_batched` for small
    // creatures, `forward_mse_scratch` for those above the 256 cap (Issue #182).
    let gpu_kernel_label = runner.kernel_label();
    let mut results = BTreeMap::new();
    for (loaded_creature, mse_sum) in loaded.iter().zip(total_mse.iter()) {
        let avg_error = *mse_sum / total_records as f64;
        let components = compute_score_components(&loaded_creature.creature)?;
        let hidden_neurons = components.hidden_neuron_count;
        let synapse_count = components.synapse_count;

        let complexity_penalty = complexity_penalty(&components, GROWTH_COST)?;

        let score = calculate_score(
            avg_error,
            &components,
            GROWTH_COST,
            loaded_creature.creature.semantic_version.as_deref(),
        )?;

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
                gpu_backend,
                read_buf_len: Some(fused_read_buf_len),
                activation_threads: Some(activation_threads),
                parallel_activation_batches: None,
                max_activation_batch_records: None,
                time_taken_secs: elapsed,
                compile_time_secs: Some(compile_time_secs),
                gpu_kernel: Some(gpu_kernel_label.to_string()),
                gpu_inflight_chunks: Some(inflight_chunks),
                gpu_dispatch_count: Some(dispatch_count),
                cost_name: cost.as_str().to_string(),
            },
        );
    }

    Ok(results)
}

/// Callback that scores one chunk of decoded packed records. The callee is
/// expected to update its own per-creature MSE running totals.
type ScoreChunkFn<'a> = dyn FnMut(&[f32], usize) -> Result<(), String> + 'a;

/// Shared head-and-compact I/O loop reused by both CPU and GPU directory-mode
/// paths. Calls `score_chunk(floats, n_records)` for each whole-record slice
/// teased out of the streamed `chunk`.
fn run_io_loop(
    chunk: &[u8],
    pending: &mut Vec<u8>,
    head: &mut usize,
    unpack_floats: &mut Vec<f32>,
    record_bytes: usize,
    score_chunk: &mut ScoreChunkFn<'_>,
) -> Result<(), String> {
    if pending.is_empty() && *head == 0 && !chunk.is_empty() {
        let aligned_len = (chunk.len() / record_bytes) * record_bytes;
        if aligned_len > 0 {
            let num_f32 = aligned_len / 4;
            let n_records = aligned_len / record_bytes;
            unpack_f32s_le(&chunk[..aligned_len], unpack_floats, num_f32);
            score_chunk(unpack_floats, n_records)?;
        }
        if aligned_len < chunk.len() {
            pending.extend_from_slice(&chunk[aligned_len..]);
        }
        return Ok(());
    }

    if !chunk.is_empty() {
        pending.extend_from_slice(chunk);
    }
    compact_pending_if_needed(pending, head);

    loop {
        let avail = pending.len() - *head;
        let complete_len = (avail / record_bytes) * record_bytes;
        if complete_len == 0 {
            break;
        }
        let num_f32 = complete_len / 4;
        let n_records = complete_len / record_bytes;
        let slice = &pending[*head..*head + complete_len];
        unpack_f32s_le(slice, unpack_floats, num_f32);
        score_chunk(unpack_floats, n_records)?;
        *head += complete_len;
    }
    if *head == pending.len() {
        pending.clear();
        *head = 0;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpack_f32s_le_decodes_exact_length_buffer() {
        // Two little-endian f32s: 3.5 and 0.0.
        let mut src = Vec::new();
        src.extend_from_slice(&3.5_f32.to_le_bytes());
        src.extend_from_slice(&0.0_f32.to_le_bytes());
        let mut dst = Vec::new();
        unpack_f32s_le(&src, &mut dst, 2);
        assert_eq!(dst, vec![3.5_f32, 0.0_f32]);
    }

    #[test]
    #[should_panic(expected = "unpack_f32s_le: src.len()")]
    fn unpack_f32s_le_rejects_short_buffer_in_release() {
        // Length 7 with n=2 means src.len() != n*4 (=8). Must panic in
        // both debug and release builds (Issue #103) — never enter the
        // unsafe loop with a short slice.
        let src = vec![0u8; 7];
        let mut dst = Vec::new();
        unpack_f32s_le(&src, &mut dst, 2);
    }

    #[test]
    #[should_panic(expected = "unpack_f32s_le: src.len()")]
    fn unpack_f32s_le_rejects_oversize_buffer() {
        let src = vec![0u8; 9];
        let mut dst = Vec::new();
        unpack_f32s_le(&src, &mut dst, 2);
    }

    #[test]
    fn workers_per_creature_one_per_when_population_meets_threads() {
        // N >= activation_threads → each creature gets exactly one worker.
        assert_eq!(workers_per_creature(8, 8), vec![1; 8]);
        assert_eq!(workers_per_creature(50, 8), vec![1; 50]);
        assert_eq!(workers_per_creature(200, 10), vec![1; 200]);
    }

    #[test]
    fn workers_per_creature_distributes_remainder_when_population_below_threads() {
        // 8 threads / 3 creatures → base=2, rem=2 → [3, 3, 2]; sum == 8.
        let w = workers_per_creature(3, 8);
        assert_eq!(w, vec![3, 3, 2]);
        assert_eq!(w.iter().sum::<usize>(), 8);

        // 10 threads / 4 creatures → base=2, rem=2 → [3, 3, 2, 2]; sum == 10.
        let w = workers_per_creature(4, 10);
        assert_eq!(w, vec![3, 3, 2, 2]);
        assert_eq!(w.iter().sum::<usize>(), 10);
    }

    #[test]
    fn workers_per_creature_single_creature_takes_all_threads() {
        assert_eq!(workers_per_creature(1, 1), vec![1]);
        assert_eq!(workers_per_creature(1, 8), vec![8]);
        assert_eq!(workers_per_creature(1, 200), vec![200]);
    }

    #[test]
    fn workers_per_creature_clamps_zero_threads_to_one() {
        // Defensive: activation_threads should already be >=1 from the
        // env parser, but a zero must not produce zero workers.
        assert_eq!(workers_per_creature(3, 0), vec![1, 1, 1]);
    }

    #[test]
    fn partition_packed_record_ranges_covers_full_buffer_with_no_overlap() {
        // 7 records / 3 workers → [3, 2, 2]; ranges should tile [0, 7*vpr).
        let vpr = 4;
        let ranges = partition_packed_record_ranges(vpr, 7, 3);
        assert_eq!(ranges.len(), 3);
        // Concatenated lengths = 7*vpr; each starts at the previous end.
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[0].end, 3 * vpr);
        assert_eq!(ranges[1].start, 3 * vpr);
        assert_eq!(ranges[1].end, 5 * vpr);
        assert_eq!(ranges[2].start, 5 * vpr);
        assert_eq!(ranges[2].end, 7 * vpr);
    }
}
