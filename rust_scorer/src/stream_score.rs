//! Chunked binary reads and fused MSE scoring for large datasets.
//!
//! Uses [`neat_core::training_bin_stream::for_each_read_chunk`] plus a `pending`
//! buffer with **head + compact**. Read tuning: **`NEAT_SCORER_READ_BYTES`**
//! (see [`crate::read_tuning::training_read_target_bytes_from_env_for_readers`]).
//!
//! Optional **multi-threaded activation** for large in-memory batches (forward-only only):
//! set **`NEAT_SCORER_ACTIVATION_THREADS`** to a value `> 1` (clamped by the host-aware
//! worker ceiling). Each worker owns its own [`CompiledNetwork`]; the caller-supplied
//! template network is cloned once per additional worker so activation/hint/trace scratch
//! buffers stay independent without paying a second `compile_creature` cost (Issue #42 —
//! `CompiledNetwork: Clone` landed upstream). Any batch with at least two whole records may
//! be split across workers (very small batches still pay Rayon scheduling cost). Summation
//! order may differ slightly (floating-point). JSON **`parallelActivationBatches`** counts
//! how many batches actually used Rayon.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use crate::cost::{CostKind, accumulate_cost_sum};
use crate::host_resources::{self, HostResources};
use crate::read_tuning::{self, max_read_bytes, training_read_target_bytes_from_env_for_readers};
use crate::sampling::SampleSpec;
use crate::stream_io::run_io_loop;
use neat_core::network::CompiledNetwork;
use neat_core::training_bin_stream::for_each_read_chunk;
use neat_core::training_data::TrainingDataConfig;
use rayon::prelude::*;

/// Parsed `NEAT_SCORER_ACTIVATION_THREADS`: missing defaults to a host-aware
/// worker count (every logical CPU on mid/large hosts; clamped on low-RAM
/// machines). Clamped to `[1, max_worker_count(host)]`.
///
/// # Examples
///
/// ```
/// use rust_scorer::stream_score::activation_worker_count_for_scorer;
///
/// // Always at least one worker and never above the host ceiling.
/// let workers = activation_worker_count_for_scorer();
/// assert!((1..=256).contains(&workers));
/// ```
pub fn activation_worker_count_for_scorer() -> usize {
    activation_worker_count_for(&host_resources::host())
}

/// Testable variant of [`activation_worker_count_for_scorer`].
pub(crate) fn activation_worker_count_for(host: &HostResources) -> usize {
    // Unset/blank/malformed all resolve to the host default; a malformed
    // value additionally warns instead of falling back silently (Issue #204).
    let default = host_resources::default_worker_count(host);
    let env = std::env::var("NEAT_SCORER_ACTIVATION_THREADS").ok();
    let (parsed, warning) = crate::env_tuning::parse_tuning_var(
        "NEAT_SCORER_ACTIVATION_THREADS",
        env.as_deref(),
        default,
        |s| s.parse::<usize>().ok(),
    );
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }
    parsed.clamp(1, host_resources::max_worker_count(host))
}

/// Aligned fused read size (same rounding as
/// [`training_read_target_bytes_from_env_for_readers`]).
///
/// When **`NEAT_SCORER_ACTIVATION_THREADS` > 1**, bumps the buffer to at least **`2 * record_bytes`**
/// (capped like the core I/O tuner) so `pending` can hold two whole records per activation batch.
/// Otherwise each read often yields only one complete record when `record_bytes` is large, and
/// parallel activation never runs (`parallelActivationBatches: 0`).
///
/// # Examples
///
/// ```
/// use rust_scorer::stream_score::effective_fused_read_buf_len;
///
/// // The buffer is always a whole number of records and holds at least one.
/// let record_bytes = 256;
/// let len = effective_fused_read_buf_len(record_bytes, 4096);
/// assert_eq!(len % record_bytes, 0);
/// assert!(len >= record_bytes);
/// ```
pub fn effective_fused_read_buf_len(record_bytes: usize, target_read_bytes: usize) -> usize {
    let rb = record_bytes.max(1);
    let worker_count = activation_worker_count_for_scorer();
    let mut len = (target_read_bytes / rb * rb).max(rb);
    if worker_count > 1 {
        len = len.max(rb.saturating_mul(2));
    }
    let capped = (max_read_bytes() / rb) * rb;
    len.min(capped.max(rb))
}

/// Sentinel `file_workers` value meaning "resolve from the environment / CPU
/// count" (Issue #529).
pub const AUTO_FILE_READ_WORKERS: usize = 0;

/// Parsed `NEAT_SCORER_FILE_THREADS`: how many `.bin` files are read and scored
/// concurrently on the forward-only fused path (Issue #529).
///
/// Missing/blank defaults to a host-aware reader count (one per logical CPU on
/// mid/large hosts, fewer on low-RAM machines), never more than there are
/// files. `1` disables parallel file reads; a corpus of one file is always
/// sequential.
///
/// # Examples
///
/// ```
/// use rust_scorer::stream_score::file_read_worker_count;
///
/// // A single-file corpus has no second file to read in parallel.
/// assert_eq!(file_read_worker_count(1), 1);
/// // Never more readers than files.
/// assert!(file_read_worker_count(4) <= 4);
/// ```
pub fn file_read_worker_count(num_files: usize) -> usize {
    file_read_worker_count_for(num_files, &host_resources::host())
}

/// Readers a **sampled** corpus read spreads its sparse fetches over
/// (NEAT-AI-Lamarck#123).
///
/// Deliberately *not* capped by the file count the way
/// [`file_read_worker_count`] is: a sampled read splits each file into record
/// windows, so a single-file corpus parallelises just as well as a 520-file one.
/// The cap matters because sparse reads only beat a sequential sweep with
/// several requests in flight — one reader is slower than reading everything.
///
/// Shares the `NEAT_SCORER_FILE_THREADS` escape hatch and the host's worker
/// ceiling with the fused reader, so there is no new per-host knob.
///
/// # Examples
///
/// ```
/// use rust_scorer::stream_score::sampled_read_worker_count;
///
/// // Always at least one reader, whatever the host reports.
/// assert!(sampled_read_worker_count() >= 1);
/// ```
pub fn sampled_read_worker_count() -> usize {
    sampled_read_worker_count_for(&host_resources::host())
}

/// Testable variant of [`sampled_read_worker_count`].
pub(crate) fn sampled_read_worker_count_for(host: &HostResources) -> usize {
    let default = host_resources::default_worker_count(host);
    let env = std::env::var("NEAT_SCORER_FILE_THREADS").ok();
    let (parsed, warning) = crate::env_tuning::parse_tuning_var(
        "NEAT_SCORER_FILE_THREADS",
        env.as_deref(),
        default,
        |s| s.parse::<usize>().ok(),
    );
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }
    parsed.clamp(1, host_resources::max_worker_count(host))
}

/// Testable variant of [`file_read_worker_count`].
pub(crate) fn file_read_worker_count_for(num_files: usize, host: &HostResources) -> usize {
    if num_files <= 1 {
        return 1;
    }
    let default = host_resources::default_worker_count(host).min(num_files);
    let env = std::env::var("NEAT_SCORER_FILE_THREADS").ok();
    let (parsed, warning) = crate::env_tuning::parse_tuning_var(
        "NEAT_SCORER_FILE_THREADS",
        env.as_deref(),
        default,
        |s| s.parse::<usize>().ok(),
    );
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }
    parsed.clamp(1, num_files.min(host_resources::max_worker_count(host)))
}

/// Activation workers *per file reader* (Issue #529).
///
/// The two parallelism axes share one CPU budget: with `readers` files in
/// flight, each reader splits its own chunks across
/// `activation_threads / readers` networks (at least one). A single reader
/// keeps the full [`activation_worker_count_for_scorer`] budget, so the
/// pre-#529 single-file behaviour is unchanged.
///
/// # Examples
///
/// ```
/// use rust_scorer::stream_score::{
///     activation_worker_count_for_scorer, activation_workers_per_file_worker,
/// };
///
/// // One reader keeps the whole activation budget.
/// assert_eq!(
///     activation_workers_per_file_worker(1),
///     activation_worker_count_for_scorer()
/// );
/// // Always at least one activation worker per reader.
/// assert!(activation_workers_per_file_worker(1000) >= 1);
/// ```
pub fn activation_workers_per_file_worker(readers: usize) -> usize {
    let total = activation_worker_count_for_scorer();
    if readers <= 1 {
        total
    } else {
        (total / readers).max(1)
    }
}

/// Per-file global record offsets, or `None` when the corpus cannot be split
/// per file (Issue #529).
///
/// Returns `None` when any file's length is not a whole number of records (a
/// record straddles the boundary, so only the continuous single-stream reader
/// can reassemble it) or when a file's metadata cannot be read — the sequential
/// reader then runs and surfaces the real I/O error with its file-index
/// diagnostic rather than this path swallowing it.
fn record_aligned_file_starts(
    bin_files: &[std::path::PathBuf],
    record_bytes: usize,
) -> Option<Vec<u64>> {
    if record_bytes == 0 {
        return None;
    }
    let record_bytes = record_bytes as u64;
    let mut starts = Vec::with_capacity(bin_files.len());
    let mut cumulative = 0_u64;
    for path in bin_files {
        let len = std::fs::metadata(path).ok()?.len();
        if !len.is_multiple_of(record_bytes) {
            return None;
        }
        starts.push(cumulative);
        cumulative += len / record_bytes;
    }
    Some(starts)
}

/// Effective number of concurrent `.bin` readers for this corpus (Issue #529).
///
/// Runs the same resolution the fused accumulator applies internally —
/// requested (or [`AUTO_FILE_READ_WORKERS`]) count, capped by the file count
/// and forced to `1` for a corpus that is not record-aligned per file — so the
/// CLI can report it without re-implementing the rules. Stats only: it reads
/// file metadata and nothing else.
///
/// # Examples
///
/// ```
/// use rust_scorer::stream_score::{resolved_file_read_workers, AUTO_FILE_READ_WORKERS};
///
/// // An empty corpus has nothing to parallelise.
/// assert_eq!(resolved_file_read_workers(&[], 16, AUTO_FILE_READ_WORKERS), 1);
/// ```
pub fn resolved_file_read_workers(
    bin_files: &[std::path::PathBuf],
    record_bytes: usize,
    requested: usize,
) -> usize {
    let starts = record_aligned_file_starts(bin_files, record_bytes);
    resolve_file_read_workers(bin_files.len(), requested, &starts)
}

/// Resolve the effective reader count: the requested (or auto) count, capped by
/// the file count and forced to `1` when the corpus is not record-aligned
/// per file (Issue #529).
fn resolve_file_read_workers(
    num_files: usize,
    requested: usize,
    file_starts: &Option<Vec<u64>>,
) -> usize {
    if num_files <= 1 || file_starts.is_none() {
        return 1;
    }
    let resolved = if requested == AUTO_FILE_READ_WORKERS {
        file_read_worker_count(num_files)
    } else {
        requested
    };
    resolved.clamp(
        1,
        num_files.min(host_resources::max_worker_count(&host_resources::host())),
    )
}

/// Per-reader read-buffer size (Issue #529). Shares one total read budget
/// across the readers so W concurrent readers never hold more buffer memory
/// than the host's aggregate read budget, while staying a whole number of
/// records.
///
/// **Issue #549:** the budget divided here is
/// [`read_tuning::aggregate_read_budget_bytes`] — the named resident read-buffer
/// budget — rather than `max_read_bytes`, the *override clamp* it borrowed
/// before. Same value on every host (that clamp's RAM tier moved across with
/// it), but the reader-count-aware default now computes from the same budget, so
/// this call only has to bound a hand-set `NEAT_SCORER_READ_BYTES`.
fn per_reader_read_buf_len(record_bytes: usize, read_buf_len: usize, readers: usize) -> usize {
    per_reader_read_buf_len_for(record_bytes, read_buf_len, readers, &host_resources::host())
}

/// Testable variant of [`per_reader_read_buf_len`].
fn per_reader_read_buf_len_for(
    record_bytes: usize,
    read_buf_len: usize,
    readers: usize,
    host: &HostResources,
) -> usize {
    let rb = record_bytes.max(1);
    let budget = read_tuning::per_reader_read_budget_bytes(host, readers).max(rb);
    let len = read_buf_len.min(budget);
    ((len / rb) * rb).max(rb)
}

/// Split `records` (layout `[inputs..., targets...]` per record) into `workers` contiguous slices.
/// `workers` must be `<= n_records` so every slice is non-empty.
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

/// Accumulate fused cost-function sums over all `.bin` files using env-tuned
/// chunked reads. Issue #121 generalises this from MSE-only to dispatch via
/// [`accumulate_cost_sum`] so every supported [`CostKind`] runs through the
/// same I/O envelope.
///
/// Returns `(loss_sum, record_count, parallel_activation_batches, max_records_per_batch, clone_time_secs)`.
/// `clone_time_secs` covers any per-worker `CompiledNetwork` clones for activation parallelism
/// (always `0.0` when `activation_threads <= 1`). The fourth value is the largest `n_records`
/// seen in one activation call (diagnostic: if it stays `1` while `parallel_activation_batches`
/// is `0`, each read chunk holds at most one full record — raise **`NEAT_SCORER_READ_BYTES`**
/// so multiple records fit in `pending` at once).
///
/// # Examples
///
/// ```no_run
/// use std::path::PathBuf;
/// use rust_scorer::cost::CostKind;
/// use rust_scorer::stream_score::accumulate_cost_sum_forward_only_fused;
/// use neat_core::creature::{compile_creature, parse_creature_json};
/// use neat_core::training_data::TrainingDataConfig;
///
/// let creature = parse_creature_json(&std::fs::read_to_string("creature.json").unwrap()).unwrap();
/// let mut network = compile_creature(&creature).unwrap();
/// let config = TrainingDataConfig {
///     num_inputs: creature.input,
///     num_outputs: creature.output,
/// };
/// let bin_files = vec![PathBuf::from("data/train.bin")];
///
/// let (loss_sum, records, _batches, _max_batch, _clone_secs) =
///     accumulate_cost_sum_forward_only_fused(
///         CostKind::Mse,
///         &bin_files,
///         &config,
///         &mut network,
///     )
///     .unwrap();
/// let mean_error = loss_sum / records as f64;
/// println!("mean error = {mean_error}");
/// ```
pub fn accumulate_cost_sum_forward_only_fused(
    cost: CostKind,
    bin_files: &[std::path::PathBuf],
    config: &TrainingDataConfig,
    network: &mut CompiledNetwork,
) -> Result<(f64, usize, usize, usize, f64), String> {
    accumulate_cost_sum_forward_only_fused_sampled(
        cost,
        bin_files,
        config,
        network,
        SampleSpec::full(),
    )
}

/// Issue #310 — record-level sub-sampling variant of
/// [`accumulate_cost_sum_forward_only_fused`].
///
/// Identical to the full-rate function except that `sample` selects a
/// deterministic, stratified subsample of the corpus (see
/// [`crate::sampling`]). `sample = SampleSpec::full()` reproduces the full-rate
/// behaviour exactly (the returned `record_count` then equals the full corpus
/// count); a sub-rate `sample` returns the loss sum and count over the **kept**
/// records only, so `loss_sum / record_count` is still the mean error over the
/// scored subset.
///
/// Issue #470 vestigial-parameter sweep: the former `_creature:
/// &CreatureExport` argument was dropped from both entry points. It had been
/// unread since the fused path started driving the pre-compiled
/// `CompiledNetwork` directly — `config` already carries the input/output
/// widths the reader needs, so the export was pure dead weight at every call
/// site.
pub(crate) fn accumulate_cost_sum_forward_only_fused_sampled(
    cost: CostKind,
    bin_files: &[std::path::PathBuf],
    config: &TrainingDataConfig,
    network: &mut CompiledNetwork,
    sample: SampleSpec,
) -> Result<(f64, usize, usize, usize, f64), String> {
    accumulate_cost_sum_forward_only_fused_sampled_with_workers(
        cost,
        bin_files,
        config,
        network,
        sample,
        AUTO_FILE_READ_WORKERS,
    )
}

/// Issue #529 — full-rate variant of
/// [`accumulate_cost_sum_forward_only_fused_sampled_with_workers`] with an
/// explicit file-reader worker count. Used by the Criterion `fused_multi_file`
/// group to A/B the sequential reader (`file_workers = 1`) against parallel
/// file reads without touching process-wide environment state.
///
/// # Examples
///
/// ```no_run
/// use std::path::PathBuf;
/// use rust_scorer::cost::CostKind;
/// use rust_scorer::stream_score::accumulate_cost_sum_forward_only_fused_with_workers;
/// use neat_core::creature::{compile_creature, parse_creature_json};
/// use neat_core::training_data::TrainingDataConfig;
///
/// let creature = parse_creature_json(&std::fs::read_to_string("creature.json").unwrap()).unwrap();
/// let mut network = compile_creature(&creature).unwrap();
/// let config = TrainingDataConfig {
///     num_inputs: creature.input,
///     num_outputs: creature.output,
/// };
/// let bin_files = vec![PathBuf::from("data/0.bin"), PathBuf::from("data/1.bin")];
///
/// // `0` resolves the worker count from the environment / CPU count.
/// let (loss_sum, records, ..) = accumulate_cost_sum_forward_only_fused_with_workers(
///     CostKind::Mse,
///     &bin_files,
///     &config,
///     &mut network,
///     2,
/// )
/// .unwrap();
/// println!("mean error = {}", loss_sum / records as f64);
/// ```
pub fn accumulate_cost_sum_forward_only_fused_with_workers(
    cost: CostKind,
    bin_files: &[std::path::PathBuf],
    config: &TrainingDataConfig,
    network: &mut CompiledNetwork,
    file_workers: usize,
) -> Result<(f64, usize, usize, usize, f64), String> {
    accumulate_cost_sum_forward_only_fused_sampled_with_workers(
        cost,
        bin_files,
        config,
        network,
        SampleSpec::full(),
        file_workers,
    )
}

/// Issue #529 — sub-sampling fused accumulator with an explicit file-reader
/// worker count.
///
/// `file_workers` is the number of `.bin` files read (and scored) concurrently;
/// pass [`AUTO_FILE_READ_WORKERS`] to resolve it from
/// [`file_read_worker_count`]. `1` reproduces the pre-#529 single-reader sweep
/// exactly. Higher values remove the sequential axis that a single reader
/// imposes: the `f32` unpack and the per-chunk fork/join barrier are per-file
/// work, so W readers unpack and score W chunks at once.
///
/// Ordering does not matter — the accumulator is a plain sum over records — but
/// the result is still *deterministic*: each file's partial loss is folded back
/// in **file order**, so the answer does not depend on which worker happened to
/// pick up which file. It can differ from the sequential sweep in the last
/// floating-point bits, because the records are grouped into different partial
/// sums (relative difference well under `1e-9`).
///
/// Falls back to the sequential sweep when the corpus is a single file, when
/// `file_workers` resolves to `1`, or when any file's length is **not** a whole
/// number of records — a record spliced across a file boundary can only be
/// reassembled by the continuous single-stream reader. (`corpus_guard`
/// rejects such a corpus at the CLI; the fallback keeps the library API
/// correct for callers that skip that guard.)
pub fn accumulate_cost_sum_forward_only_fused_sampled_with_workers(
    cost: CostKind,
    bin_files: &[std::path::PathBuf],
    config: &TrainingDataConfig,
    network: &mut CompiledNetwork,
    sample: SampleSpec,
    file_workers: usize,
) -> Result<(f64, usize, usize, usize, f64), String> {
    let record_bytes = config.bytes_per_record();
    if record_bytes == 0 {
        return Err("Invalid record byte length (zero)".to_string());
    }

    // Issue #121: validate the cost is dispatchable up-front. Every built-in
    // cost dispatches today (Issue #134 wired the last one, `CATEGORICAL_ERROR`);
    // the probe stays so a future kernel-only cost that surfaces an error here
    // is caught before any bytes are read. Issue #200: the inner par_iter now
    // propagates any per-chunk error via `?` rather than `.expect`, so a
    // content-dependent failure also returns a clean `Err`.
    accumulate_cost_sum(
        cost,
        network,
        &[],
        config.num_inputs,
        config.num_outputs,
        true,
    )?;

    // For multi-threaded activation, each worker needs an independent `CompiledNetwork`
    // (separate activation/hint/trace buffers). `CompiledNetwork: Clone` landed upstream
    // (NEAT-AI-core#11), so we now clone the caller-supplied template once per extra worker
    // instead of paying a second `compile_creature` per thread (Issue #42).
    //
    // Issue #529: the same clone budget is now spread over two axes — one
    // `CompiledNetwork` per (file reader × activation worker) pair — so total
    // concurrency (and total clone cost) stays at the CPU count.
    let file_start_records = record_aligned_file_starts(bin_files, record_bytes);
    let readers = resolve_file_read_workers(bin_files.len(), file_workers, &file_start_records);
    let activation_workers = activation_workers_per_file_worker(readers);

    // Issue #549: every reader holds its own read buffer, so the chunk default is
    // sized for the *aggregate* `readers × chunk` footprint — resolved here,
    // after the reader count is known.
    let target_read_bytes = training_read_target_bytes_from_env_for_readers(record_bytes, readers);
    let read_buf_len = effective_fused_read_buf_len(record_bytes, target_read_bytes);

    let clone_started = Instant::now();
    let total_clones = readers * activation_workers;
    // One clone per worker network; the sequential single-threaded case keeps
    // using the caller's network directly and clones nothing.
    let mut worker_nets: Vec<Vec<CompiledNetwork>> = if total_clones > 1 {
        (0..readers)
            .map(|_| (0..activation_workers).map(|_| network.clone()).collect())
            .collect()
    } else {
        Vec::new()
    };
    let clone_time_secs = if total_clones > 1 {
        clone_started.elapsed().as_secs_f64()
    } else {
        0.0
    };

    if readers <= 1 {
        // Sequential whole-corpus sweep — the pre-#529 behaviour, kept intact
        // for single-file corpora and for a corpus whose records straddle file
        // boundaries.
        let nets_slice: &mut [CompiledNetwork] = if worker_nets.is_empty() {
            std::slice::from_mut(network)
        } else {
            &mut worker_nets[0]
        };
        let mut scorer = ChunkScorer::new(cost, config, nets_slice);
        let mut pending: Vec<u8> = Vec::new();
        let mut head: usize = 0;
        let mut unpack_floats: Vec<f32> = Vec::new();
        // Issue #310: one stateful sampler threads the global record index across
        // every streamed chunk so the kept set is independent of chunk boundaries.
        let mut sampler = sample.sampler();
        for_each_read_chunk(bin_files, read_buf_len, |chunk| {
            run_io_loop(
                chunk,
                &mut pending,
                &mut head,
                &mut unpack_floats,
                record_bytes,
                &mut sampler,
                &mut |floats: &mut Vec<f32>, n: usize| scorer.score(floats, n),
            )
        })?;

        if head != pending.len() {
            return Err(format!(
                "Trailing {} bytes (incomplete record) after reading all training files",
                pending.len() - head
            ));
        }

        return Ok((
            scorer.loss_sum,
            scorer.records,
            scorer.parallel_activation_batches,
            scorer.max_records_per_activation_batch,
            clone_time_secs,
        ));
    }

    // --- Parallel file reads (Issue #529) ---------------------------------
    //
    // `record_aligned_file_starts` gave every file the global index of its
    // first record, so each reader can seed its own sampler and the kept set
    // stays identical to the sequential sweep.
    let starts = file_start_records.as_ref().ok_or_else(|| {
        "Internal error: parallel reads without per-file record offsets".to_string()
    })?;
    let per_reader_read_buf_len = per_reader_read_buf_len(record_bytes, read_buf_len, readers);

    // Dynamic work queue: readers pull the next file index rather than taking a
    // fixed slice, so uneven file sizes cannot leave one reader trailing.
    let next_file = AtomicUsize::new(0);
    let per_reader: Result<Vec<Vec<FileScore>>, String> = worker_nets
        .par_iter_mut()
        .map(|nets| {
            let mut mine: Vec<FileScore> = Vec::new();
            loop {
                let index = next_file.fetch_add(1, Ordering::Relaxed);
                if index >= bin_files.len() {
                    break;
                }
                mine.push(score_one_file(
                    cost,
                    &bin_files[index],
                    index,
                    starts[index],
                    config,
                    nets,
                    sample,
                    record_bytes,
                    per_reader_read_buf_len,
                )?);
            }
            Ok(mine)
        })
        .collect();

    let mut scores: Vec<FileScore> = per_reader?.into_iter().flatten().collect();
    // Fold in file order so the total is independent of the scheduling order.
    scores.sort_unstable_by_key(|s| s.file_index);

    let mut total_loss_sum = 0.0_f64;
    let mut total_records = 0_usize;
    let mut parallel_activation_batches = 0_usize;
    let mut max_records_per_activation_batch = 0_usize;
    for s in &scores {
        total_loss_sum += s.loss_sum;
        total_records += s.records;
        parallel_activation_batches += s.parallel_activation_batches;
        max_records_per_activation_batch =
            max_records_per_activation_batch.max(s.max_records_per_activation_batch);
    }

    Ok((
        total_loss_sum,
        total_records,
        parallel_activation_batches,
        max_records_per_activation_batch,
        clone_time_secs,
    ))
}

/// One file's contribution to the fused totals (Issue #529).
struct FileScore {
    file_index: usize,
    loss_sum: f64,
    records: usize,
    parallel_activation_batches: usize,
    max_records_per_activation_batch: usize,
}

/// Stream and score a single `.bin` file, seeding the sampler at that file's
/// first global record index (Issue #529).
#[allow(clippy::too_many_arguments)]
fn score_one_file(
    cost: CostKind,
    path: &std::path::Path,
    file_index: usize,
    start_record: u64,
    config: &TrainingDataConfig,
    nets: &mut [CompiledNetwork],
    sample: SampleSpec,
    record_bytes: usize,
    read_buf_len: usize,
) -> Result<FileScore, String> {
    let mut scorer = ChunkScorer::new(cost, config, nets);
    let mut pending: Vec<u8> = Vec::new();
    let mut head: usize = 0;
    let mut unpack_floats: Vec<f32> = Vec::new();
    let mut sampler = sample.sampler_at(start_record);
    let only_file = [path.to_path_buf()];

    for_each_read_chunk(&only_file, read_buf_len, |chunk| {
        run_io_loop(
            chunk,
            &mut pending,
            &mut head,
            &mut unpack_floats,
            record_bytes,
            &mut sampler,
            &mut |floats: &mut Vec<f32>, n: usize| scorer.score(floats, n),
        )
    })?;

    // Fail loud rather than dropping a partial record: the reader only gets
    // here for a corpus pre-checked as record-aligned, so a remainder means the
    // file changed underneath us mid-run.
    if head != pending.len() {
        return Err(format!(
            "Trailing {} bytes (incomplete record) in training file {}",
            pending.len() - head,
            path.display()
        ));
    }

    Ok(FileScore {
        file_index,
        loss_sum: scorer.loss_sum,
        records: scorer.records,
        parallel_activation_batches: scorer.parallel_activation_batches,
        max_records_per_activation_batch: scorer.max_records_per_activation_batch,
    })
}

/// Scores the whole-record slices `run_io_loop` tees up, optionally splitting
/// each batch across `nets` with Rayon (Issue #203 loop, Issue #42 activation
/// parallelism), and carries the running totals for one corpus sweep.
struct ChunkScorer<'a> {
    cost: CostKind,
    num_inputs: usize,
    num_outputs: usize,
    values_per_record: usize,
    nets: &'a mut [CompiledNetwork],
    loss_sum: f64,
    records: usize,
    parallel_activation_batches: usize,
    max_records_per_activation_batch: usize,
}

impl<'a> ChunkScorer<'a> {
    fn new(cost: CostKind, config: &TrainingDataConfig, nets: &'a mut [CompiledNetwork]) -> Self {
        Self {
            cost,
            num_inputs: config.num_inputs,
            num_outputs: config.num_outputs,
            values_per_record: config.num_inputs + config.num_outputs,
            nets,
            loss_sum: 0.0,
            records: 0,
            parallel_activation_batches: 0,
            max_records_per_activation_batch: 0,
        }
    }

    /// Score one whole-record slice. Issue #121 dispatches every supported cost
    /// through `accumulate_cost_sum`; Issue #200 propagates a per-slice cost
    /// error via `?` rather than `.expect` across a Rayon worker.
    fn score(&mut self, floats: &[f32], n_records: usize) -> Result<(), String> {
        if floats.len() != n_records * self.values_per_record {
            return Err("Internal float unpack length mismatch".to_string());
        }
        self.max_records_per_activation_batch =
            self.max_records_per_activation_batch.max(n_records);
        self.records += n_records;

        let effective_workers = self.nets.len().min(n_records).max(1);
        let chunk_sum: f64 = if effective_workers > 1 {
            self.parallel_activation_batches += 1;
            let slices = partition_packed_records(
                floats,
                self.values_per_record,
                n_records,
                effective_workers,
            );
            let (cost, num_inputs, num_outputs) = (self.cost, self.num_inputs, self.num_outputs);
            let partials: Result<Vec<f64>, String> = self.nets[..effective_workers]
                .par_iter_mut()
                .zip(slices)
                .map(|(net, slice)| {
                    accumulate_cost_sum(cost, net, slice, num_inputs, num_outputs, true)
                })
                .collect();
            partials?.into_iter().sum()
        } else {
            accumulate_cost_sum(
                self.cost,
                &mut self.nets[0],
                floats,
                self.num_inputs,
                self.num_outputs,
                true,
            )?
        };
        self.loss_sum += chunk_sum;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // `unpack_f32s_le` (and its Issue #103 OOB `should_panic` tests) moved to
    // the shared `crate::stream_io` module in Issue #203; the canonical copy of
    // those tests now lives there.
    use super::{
        AUTO_FILE_READ_WORKERS, partition_packed_records, per_reader_read_buf_len,
        per_reader_read_buf_len_for, record_aligned_file_starts, resolve_file_read_workers,
    };
    use crate::host_resources::{self, GIB, HostResources};
    use crate::read_tuning;
    use std::io::Write;

    #[test]
    fn partition_packed_records_covers_all_and_balances() {
        let values_per_record = 3_usize;
        let n_records = 10_usize;
        let workers = 4_usize;
        let records: Vec<f32> = (0..(n_records * values_per_record))
            .map(|i| i as f32)
            .collect();
        let parts = partition_packed_records(&records, values_per_record, n_records, workers);
        assert_eq!(parts.len(), workers);
        assert_eq!(parts.iter().map(|s| s.len()).sum::<usize>(), records.len());
        // 10 / 4 => lengths 3,3,2,2 in floats => 9,9,6,6
        assert_eq!(parts[0].len(), 9);
        assert_eq!(parts[1].len(), 9);
        assert_eq!(parts[2].len(), 6);
        assert_eq!(parts[3].len(), 6);
    }

    /// Write `sizes[n]` bytes into shard `n` and return the paths.
    fn write_shards(dir: &std::path::Path, sizes: &[usize]) -> Vec<std::path::PathBuf> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, &len)| {
                let path = dir.join(format!("{i}.bin"));
                let mut f = std::fs::File::create(&path).expect("create shard");
                f.write_all(&vec![0_u8; len]).expect("write shard");
                path
            })
            .collect()
    }

    #[test]
    fn file_starts_accumulate_record_offsets_across_shards() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // record_bytes = 16 → 3, 0 and 5 records.
        let files = write_shards(tmp.path(), &[48, 0, 80]);
        let starts = record_aligned_file_starts(&files, 16).expect("aligned corpus");
        assert_eq!(starts, vec![0, 3, 3]);
    }

    #[test]
    fn file_starts_reject_a_misaligned_shard() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let files = write_shards(tmp.path(), &[48, 40]);
        assert!(
            record_aligned_file_starts(&files, 16).is_none(),
            "a shard holding a partial record cannot be framed independently"
        );
    }

    #[test]
    fn file_starts_reject_an_unreadable_shard() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut files = write_shards(tmp.path(), &[48]);
        files.push(tmp.path().join("missing.bin"));
        assert!(
            record_aligned_file_starts(&files, 16).is_none(),
            "an unreadable shard must not be assumed empty"
        );
    }

    #[test]
    fn resolve_clamps_readers_to_the_file_count_and_alignment() {
        let starts = Some(vec![0_u64, 10, 20, 30]);
        assert_eq!(resolve_file_read_workers(4, 16, &starts), 4);
        assert_eq!(resolve_file_read_workers(4, 2, &starts), 2);
        assert_eq!(resolve_file_read_workers(4, 1, &starts), 1);
        // A single-file corpus and a misaligned corpus both stay sequential.
        assert_eq!(resolve_file_read_workers(1, 8, &Some(vec![0])), 1);
        assert_eq!(resolve_file_read_workers(4, 8, &None), 1);
        // Auto never exceeds the file count either.
        assert!(resolve_file_read_workers(4, AUTO_FILE_READ_WORKERS, &starts) <= 4);
    }

    #[test]
    fn per_reader_read_buf_shares_one_total_budget() {
        let record_bytes = 9848_usize; // production record width
        let single = 32 * 1024 * 1024_usize;

        // One reader keeps the full (record-aligned) buffer.
        assert_eq!(
            per_reader_read_buf_len(record_bytes, single, 1),
            (single / record_bytes) * record_bytes
        );

        for readers in [2_usize, 8, 26, 64] {
            let per = per_reader_read_buf_len(record_bytes, single, readers);
            assert_eq!(per % record_bytes, 0, "buffer must be record-aligned");
            assert!(per >= record_bytes, "buffer must hold at least one record");
            assert!(
                // Issue #549: the shared budget is the named aggregate read
                // budget; before, this borrowed the `max_read_bytes` override
                // clamp, which is the same value on every host below 64 GiB.
                per * readers <= read_tuning::aggregate_read_budget_bytes(&host_resources::host()),
                "{readers} readers x {per} B exceeds the shared read budget"
            );
        }
    }

    /// Issue #549: the same budget on a grid of synthetic fleet hosts, so the
    /// invariant is checked off whatever machine happens to run the suite.
    #[test]
    fn per_reader_read_buf_shares_one_budget_on_every_fleet_host() {
        let record_bytes = 9848_usize;
        for (cpus, ram_gib) in [
            (8, 8_u64),
            (10, 16),
            (12, 24),
            (10, 32),
            (24, 64),
            (24, 192),
        ] {
            let host = HostResources::synthetic(cpus, Some(ram_gib * GIB));
            let budget = read_tuning::aggregate_read_budget_bytes(&host);
            for readers in [1_usize, 2, 10, 12, 24, 26] {
                // Request far more than any budget allows, so the split is what
                // bounds the result.
                let per =
                    per_reader_read_buf_len_for(record_bytes, 256 * 1024 * 1024, readers, &host);
                assert_eq!(per % record_bytes, 0, "buffer must be record-aligned");
                assert!(per >= record_bytes, "buffer must hold at least one record");
                assert!(
                    per * readers <= budget,
                    "{cpus}c/{ram_gib} GiB: {readers} readers x {per} B exceeds the \
                     {budget} B aggregate read budget"
                );
            }
        }
    }

    /// Issue #549: the **shipped resident buffer per reader** on every fleet
    /// tier, in bytes, as `(label, logical cpus, RAM GiB, per-reader buffer)`.
    ///
    /// These are the values the pre-#549 two-stage sizing produced (record-size
    /// tier chosen blind, then divided across the readers by
    /// `per_reader_read_buf_len`). Making the default reader-aware moved the
    /// arithmetic, not the answer — this table is the proof, and a retune of any
    /// tier must land here with before/after benchmark evidence (see the
    /// [Performance Task Workflow](../../CONTRIBUTING.md#performance-task-workflow)).
    const SHIPPED_PER_READER_BUFFER: [(&str, usize, u64, usize); 8] = [
        ("M1 8 GB", 8, 8, 8_380_648),
        ("M4 16 GB", 10, 16, 6_706_488),
        ("x86 Linux 16 GB", 8, 16, 8_380_648),
        ("M4 24 GB", 10, 24, 6_706_488),
        ("M4 Pro 24 GB", 12, 24, 5_583_816),
        ("M1 Max 32 GB", 10, 32, 6_706_488),
        ("M2 Ultra 64 GB", 24, 64, 11_177_480),
        ("M2 Ultra 192 GB", 24, 192, 11_177_480),
    ];

    #[test]
    fn shipped_per_reader_buffer_is_unchanged_by_the_reader_aware_default() {
        // Production record width and shard count (Issue #529): 26 files, so the
        // reader count is the host's worker default capped at 26.
        let record_bytes = 9848_usize;
        for (label, cpus, ram_gib, expected) in SHIPPED_PER_READER_BUFFER {
            let host = HostResources::synthetic(cpus, Some(ram_gib * GIB));
            let readers = host_resources::default_worker_count(&host).min(26);
            let target =
                read_tuning::default_training_read_bytes_for_readers(record_bytes, &host, readers);
            // `effective_fused_read_buf_len` reads the *running* host for the
            // activation-worker bump and the flat override clamp, both of which
            // leave a production-width buffer alone.
            let buf = super::effective_fused_read_buf_len(record_bytes, target);
            let per = per_reader_read_buf_len_for(record_bytes, buf, readers, &host);
            assert_eq!(
                per, expected,
                "{label}: {readers} readers must still hold {expected} B each, got {per} B"
            );
            assert_eq!(per % record_bytes, 0, "{label}: whole records only");
        }
    }

    #[test]
    fn per_reader_read_buf_never_grows_the_request() {
        // A small request stays small no matter how few readers there are.
        let record_bytes = 40_usize;
        assert_eq!(per_reader_read_buf_len(record_bytes, 4000, 2), 4000);
        // ...and always holds at least one whole record.
        assert_eq!(per_reader_read_buf_len(record_bytes, 10, 64), record_bytes);
    }
}
