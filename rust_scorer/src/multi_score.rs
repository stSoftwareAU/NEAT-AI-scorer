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
//!
//! ## Recurrent creatures (Issue #579)
//!
//! A directory may mix `forwardOnly: true` and `forwardOnly: false` creatures.
//! Each worker carries its own creature's flag into `accumulate_cost_sum`, so a
//! recurrent creature is scored exactly as the single-creature path scores it:
//! `forward_only = false` makes the upstream kernel call
//! `CompiledNetwork::reset_state` before every record, which keeps records
//! independent and therefore keeps the per-worker record partition above valid.
//! The cost is that recurrent creatures lose the 8-way/4-way SIMD batch
//! kernels (upstream gates both on `forward_only`) and fall to the scalar
//! per-record scan — several times slower for **those creatures only**;
//! forward-only creatures sharing the batch are untouched.
//!
//! ## Task granularity (Issue #537)
//!
//! `NEAT_SCORER_WORKER_SPLIT=k` multiplies that worker budget by `k`, so each
//! creature's chunk is split into ~`k` times as many record sub-ranges and a
//! straggling task exposes ~1/`k` of the previous tail. `k = 1` is the shipped
//! default and reproduces the partition above exactly; the total is still
//! clamped to the host `max_worker_count` RAM ceiling (one `CompiledNetwork`
//! clone per worker). Raising `k` changes each worker's record count, which
//! re-associates a creature's f64 partial sums *and* selects a different
//! 8-record / 4-record / scalar SIMD path in the upstream loss kernels. The
//! latter dominates: it regroups the f32 activations, so scores move by
//! f32-rounding amounts (~1e-9 relative in practice), never more.

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

use crate::corpus_guard::assert_records_aligned;
use crate::cost::{CostKind, accumulate_cost_sum};
use crate::gpu::forward_mse_batched::{
    DirectoryGpuRunners, GpuPrepareError, build_batched_network_data,
    effective_directory_gpu_inflight,
};
use crate::gpu::{GpuBackendLabel, GpuContext};
// Directory scoring drives one continuous `for_each_read_chunk` sweep (a single
// read buffer), so it takes the single-reader read default — Issue #549's
// aggregate `readers × chunk` bound applies to the multi-reader fused path.
use crate::creature_width::validate_creature_width;
use crate::read_tuning::{training_read_backend_label, training_read_target_bytes_from_env};
use crate::sampling::SampleSpec;
use crate::scoring::{ScoreResult, calculate_score, complexity_penalty, compute_score_components};
use crate::stream_io::{FloatBufPool, run_io_loop};
use crate::stream_score::{activation_worker_count_for_scorer, effective_fused_read_buf_len};

/// Keep aligned with main scorer formula.
const GROWTH_COST: f64 = 0.000_000_1;

/// Instrumentation for the "single pass over training data" invariant
/// (Issue #3236).
///
/// Every multi-creature batch scoring path in this module sweeps the training
/// corpus **exactly once** via
/// [`neat_core::training_bin_stream::for_each_read_chunk`], scoring the whole
/// batch of creatures in that single pass. This process-global counter lets the
/// `single_pass_assertion` integration test observe how many sweeps a scoring
/// invocation performs and fail loudly if a future change reintroduces
/// per-creature re-reads of the training data (which would make the sweep count
/// scale with creature count instead of staying flat at `1`).
///
/// Usage from a test: call [`training_pass_probe::reset`], run one scoring
/// invocation, then assert [`training_pass_probe::count`] equals `1`.
///
/// Production overhead is a single atomic increment per whole scoring run —
/// negligible against a full training-corpus sweep.
pub mod training_pass_probe {
    use std::sync::atomic::{AtomicU64, Ordering};

    static TRAINING_DATA_SWEEPS: AtomicU64 = AtomicU64::new(0);

    /// Reset the sweep counter to zero. Call immediately before a scoring
    /// invocation whose sweep count you want to observe.
    pub fn reset() {
        TRAINING_DATA_SWEEPS.store(0, Ordering::SeqCst);
    }

    /// Number of full training-data sweeps recorded since the last [`reset`].
    pub fn count() -> u64 {
        TRAINING_DATA_SWEEPS.load(Ordering::SeqCst)
    }

    /// Record one full sweep over the training corpus. Called immediately
    /// before each `for_each_read_chunk` invocation in the batch paths.
    pub(crate) fn record_sweep() {
        TRAINING_DATA_SWEEPS.fetch_add(1, Ordering::SeqCst);
    }
}

/// Instrumentation for the "compile each creature exactly once" invariant
/// (Issue #42, guarded by Issue #355).
///
/// The batch scoring paths compile every loaded creature **once** and clone the
/// resulting [`neat_core::network::CompiledNetwork`] for any additional workers.
/// This process-global counter lets an integration test observe how many
/// `compile_creature` calls a scoring invocation performs and fail loudly if a
/// future change reintroduces the pre-#42 per-worker recompile (which would make
/// the compile count scale with total worker count instead of staying flat at
/// the creature count `N`).
///
/// Usage from a test: call [`compile_probe::reset`], run one scoring
/// invocation, then assert [`compile_probe::count`] equals the creature count.
///
/// This is a behavioural (WHAT) replacement for the machine-dependent
/// wall-clock threshold that previously guarded the same regression: it asserts
/// the invariant the spec requires (compiled exactly once per creature) rather
/// than a duration the current implementation happens to achieve.
pub mod compile_probe {
    use std::sync::atomic::{AtomicU64, Ordering};

    static CREATURE_COMPILES: AtomicU64 = AtomicU64::new(0);

    /// Reset the compile counter to zero. Call immediately before a scoring
    /// invocation whose compile count you want to observe.
    pub fn reset() {
        CREATURE_COMPILES.store(0, Ordering::SeqCst);
    }

    /// Number of `compile_creature` calls recorded since the last [`reset`].
    pub fn count() -> u64 {
        CREATURE_COMPILES.load(Ordering::SeqCst)
    }

    /// Record one `compile_creature` call in a batch scoring path. Called
    /// immediately before each such compile (not the CPU-only pre-flight
    /// hostability/topology probes, which are not part of the scoring pass).
    pub(crate) fn record_compile() {
        CREATURE_COMPILES.fetch_add(1, Ordering::SeqCst);
    }
}

struct LoadedCreature {
    key: String,
    path: PathBuf,
    creature: CreatureExport,
}

/// Why one creature in a directory could not be scored (GRQ#4387).
///
/// Every variant is a fault of **that one file** — never of the run — so the
/// directory path isolates it and scores the rest of the batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CreatureFailureKind {
    /// The `.json` file could not be read off disk.
    Read,
    /// The bytes are not a parseable creature export.
    Parse,
    /// `input` / `output` failed the observation-width guard (Issue #571).
    Width,
    /// The creature's `(input, output)` disagrees with the rest of the batch.
    Shape,
    /// `compile_creature` refused the topology (e.g. a duplicate synapse).
    Compile,
    /// Scoring produced a value the score maths refuses (NaN, negative error).
    Score,
}

/// One creature that could not be scored, emitted in place of its
/// [`ScoreResult`] so the rest of the directory still reports scores
/// (GRQ#4387).
///
/// `failed: true` is the discriminator batch consumers branch on: a
/// [`ScoreResult`] never carries it, so an entry with `failed` is
/// unambiguously an offender rather than a creature that merely scored badly.
/// `message` is the scorer's own wording — the same string the pre-GRQ#4387
/// whole-batch abort printed — so stderr-scraping consumers keep working.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatureFailure {
    /// Always `true`. Present so a consumer can tell an offender entry from a
    /// score entry without knowing the full `ScoreResult` field set.
    pub failed: bool,
    /// Machine-readable classification of the fault.
    pub reason: CreatureFailureKind,
    /// Human-readable detail, naming the offending file.
    pub message: String,
}

impl CreatureFailure {
    fn new(reason: CreatureFailureKind, message: String) -> Self {
        Self {
            failed: true,
            reason,
            message,
        }
    }
}

/// Outcome of an isolating directory-mode run (GRQ#4387).
///
/// `scored` holds every creature the scorer could score; `failed` holds the
/// offenders in discovery order (load order first, then compile, then score).
/// The two key sets are disjoint and together cover every `.json` file in the
/// directory, so the serialised map is always complete — a consumer never has
/// to guess why a stem went missing.
#[derive(Debug, Default)]
pub struct DirectoryScores {
    /// Creatures that scored, keyed by filename stem.
    pub scored: BTreeMap<String, ScoreResult>,
    /// Offenders in discovery order: `(filename stem, failure)`.
    pub failed: Vec<(String, CreatureFailure)>,
}

impl DirectoryScores {
    /// True when at least one creature in the directory could not be scored.
    pub fn has_failures(&self) -> bool {
        !self.failed.is_empty()
    }
}

impl serde::Serialize for DirectoryScores {
    /// Serialise as one flat JSON object keyed by stem, mixing score entries
    /// and offender entries. Emitting a single map (rather than two nested
    /// ones) keeps the directory-mode stdout contract unchanged for every
    /// creature that scored.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.scored.len() + self.failed.len()))?;
        for (key, value) in &self.scored {
            map.serialize_entry(key, value)?;
        }
        for (key, value) in &self.failed {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// Collects per-creature faults, or — in strict mode — turns the first one
/// into the run's error.
///
/// The two directory-mode contracts differ *only* in what happens at each
/// isolation point, so both run the same code path and this sink decides.
/// Strict mode therefore keeps the pre-GRQ#4387 ordering exactly: a bad
/// creature still aborts before the training corpus is touched.
struct FailureSink {
    isolate: bool,
    failed: Vec<(String, CreatureFailure)>,
}

impl FailureSink {
    fn isolating() -> Self {
        Self {
            isolate: true,
            failed: Vec::new(),
        }
    }

    fn strict() -> Self {
        Self {
            isolate: false,
            failed: Vec::new(),
        }
    }

    /// Record one creature's fault. Returns `Err(message)` under strict mode so
    /// the caller's `?` aborts the run, exactly as it did before isolation.
    fn record(
        &mut self,
        key: &str,
        reason: CreatureFailureKind,
        message: String,
    ) -> Result<(), String> {
        if !self.isolate {
            return Err(message);
        }
        self.failed
            .push((key.to_string(), CreatureFailure::new(reason, message)));
        Ok(())
    }
}

/// Wording for a run that lost every creature: the first offender's message,
/// which is exactly what the pre-GRQ#4387 aborting path reported.
fn first_failure_message(failed: &[(String, CreatureFailure)]) -> String {
    match failed.first() {
        Some((_, failure)) => failure.message.clone(),
        // Unreachable: the loader errors on an empty directory, so an empty
        // survivor list always comes with at least one recorded offender.
        None => "No creature in the directory could be scored".to_string(),
    }
}

/// Collapse an isolating run back to the strict, pre-GRQ#4387 contract: the
/// first offender in discovery order becomes the whole run's error, with the
/// exact wording the aborting path used.
fn strict_scores(scores: DirectoryScores) -> Result<BTreeMap<String, ScoreResult>, String> {
    match scores.failed.first() {
        Some((_, failure)) => Err(failure.message.clone()),
        None => Ok(scores.scored),
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

/// Strict loader: the first bad creature fails the whole directory.
///
/// Kept for the GPU pre-flight and topology probes, which have no way to
/// report a per-creature offender. The isolating scoring path uses
/// [`load_creatures_from_dir_isolated`].
fn load_creatures_from_dir(creatures_dir: &Path) -> Result<Vec<LoadedCreature>, String> {
    load_creatures_from_dir_into(creatures_dir, &mut FailureSink::strict())
}

/// Load every `.json` creature in `creatures_dir`, routing per-file faults
/// through `sink` (GRQ#4387).
///
/// Only **run-level** faults — the path is not a directory, the directory
/// cannot be read, it holds no `.json` file at all, or a file has no usable
/// filename stem — fail the whole load outright, because none of them can be
/// attributed to one creature the caller could drop.
fn load_creatures_from_dir_into(
    creatures_dir: &Path,
    sink: &mut FailureSink,
) -> Result<Vec<LoadedCreature>, String> {
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
        // The stem is the map key every report — score or failure — is filed
        // under, so a file without one cannot be isolated: it stays a
        // run-level error.
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

        let creature_json = match fs::read_to_string(&path) {
            Ok(json) => json,
            Err(e) => {
                sink.record(
                    &key,
                    CreatureFailureKind::Read,
                    format!("Failed to read creature file '{}': {e}", path.display()),
                )?;
                continue;
            }
        };

        let creature = match parse_creature_json(&creature_json) {
            Ok(creature) => creature,
            Err(e) => {
                sink.record(
                    &key,
                    CreatureFailureKind::Parse,
                    format!("Failed parsing creature '{}': {e}", path.display()),
                )?;
                continue;
            }
        };

        // Issue #571: the top-level counts are the observation width; a
        // `< 1` count is never re-derived from `neurons` and never defaulted.
        // GRQ#4387 narrowed the blast radius from the whole directory to this
        // one file — the guard itself is unchanged.
        if let Err(e) = validate_creature_width(&creature) {
            sink.record(
                &key,
                CreatureFailureKind::Width,
                format!("Creature '{}': {e}", path.display()),
            )?;
            continue;
        }
        // Issue #579: `forwardOnly=false` is accepted here. The per-chunk hot
        // loop threads each creature's own flag into `accumulate_cost_sum`, so
        // a recurrent creature is scored with the per-record state reset the
        // single-creature path applies — no state leaks across records.

        loaded.push(LoadedCreature {
            key,
            path,
            creature,
        });
    }

    // The batch shape is defined by the first creature that loaded; any
    // creature disagreeing with it is the offender, not the batch.
    if let Some(first) = loaded.first() {
        let expected_in = first.creature.input;
        let expected_out = first.creature.output;
        let mut kept = Vec::with_capacity(loaded.len());
        for c in loaded.drain(..) {
            if c.creature.input == expected_in && c.creature.output == expected_out {
                kept.push(c);
                continue;
            }
            sink.record(
                &c.key,
                CreatureFailureKind::Shape,
                format!(
                    "Creature '{}' has input/output=({},{}) but expected ({},{}); all creatures in directory mode must share the same shape",
                    c.path.display(),
                    c.creature.input,
                    c.creature.output,
                    expected_in,
                    expected_out
                ),
            )?;
        }
        loaded = kept;
    }

    Ok(loaded)
}

/// Environment override for the directory-mode task-granularity multiplier
/// (Issue #537). Unset (or invalid) keeps the pre-#537 granularity.
pub(crate) const WORKER_SPLIT_ENV: &str = "NEAT_SCORER_WORKER_SPLIT";

/// Shipped granularity multiplier when [`WORKER_SPLIT_ENV`] is unset.
pub(crate) const DEFAULT_WORKER_SPLIT: usize = 1;

/// Ceiling for the multiplier, so a typo'd override cannot ask for a
/// pathological number of `CompiledNetwork` clones before the
/// `max_worker_count` RAM clamp is even consulted.
const MAX_WORKER_SPLIT: usize = 64;

/// Resolve the granularity multiplier from the environment (Issue #537).
///
/// `k = 1` is the shipped default and reproduces the pre-#537 partition
/// exactly. `k > 1` splits every creature's chunk into roughly `k` times as
/// many record sub-ranges, so the Rayon pool sees finer tasks and the tail of
/// each chunk idles fewer threads.
fn worker_split_factor() -> usize {
    let env = std::env::var(WORKER_SPLIT_ENV).ok();
    let (parsed, warning) = crate::env_tuning::parse_tuning_var(
        WORKER_SPLIT_ENV,
        env.as_deref(),
        DEFAULT_WORKER_SPLIT,
        |s| s.parse::<usize>().ok().filter(|&v| v > 0),
    );
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }
    parsed.clamp(1, MAX_WORKER_SPLIT)
}

/// Pre-#537 per-creature worker counts — the reference the `split = 1` parity
/// test compares against (the scoring path calls
/// [`workers_per_creature_split`] directly).
///
/// When `n_creatures >= activation_threads` each creature gets exactly one
/// worker (the inner record split is dropped); otherwise the thread budget is
/// spread across creatures so the total worker count equals
/// `activation_threads`.
#[cfg(test)]
fn workers_per_creature(n_creatures: usize, activation_threads: usize) -> Vec<usize> {
    workers_per_creature_split(
        n_creatures,
        activation_threads,
        DEFAULT_WORKER_SPLIT,
        usize::MAX,
    )
}

/// Compute per-creature worker counts at granularity multiplier `split`
/// (Issue #537).
///
/// The `split = 1` budget is the pre-#537 one: `max(n_creatures,
/// activation_threads)` workers in total. A larger `split` multiplies that
/// budget, so each creature's chunk is partitioned into more, smaller record
/// sub-ranges and a straggling task exposes ~1/`split` of the previous tail.
/// The total is clamped to `max_total_workers` (the host RAM ceiling — one
/// `CompiledNetwork` clone is held per worker), but never below one worker per
/// creature, which is the irreducible minimum.
///
/// Every creature's workers are contiguous and folded back in worker order, so
/// the per-creature partial sums are added in a fixed order and the result
/// stays deterministic for a given `split`.
fn workers_per_creature_split(
    n_creatures: usize,
    activation_threads: usize,
    split: usize,
    max_total_workers: usize,
) -> Vec<usize> {
    debug_assert!(n_creatures > 0);
    let threads = activation_threads.max(1);
    let split = split.max(1);
    let target = threads
        .max(n_creatures)
        .saturating_mul(split)
        .min(max_total_workers.max(n_creatures));
    let base = target / n_creatures;
    let rem = target % n_creatures;
    (0..n_creatures)
        .map(|i| (base + usize::from(i < rem)).max(1))
        .collect()
}

/// Running snapshot of one still-active creature's score, handed to the
/// early-exit callback after each scored chunk (Issue #308).
///
/// The directory-mode batch path sweeps the training corpus exactly once and
/// scores every creature in that single pass. After each scored chunk a
/// registered callback receives one [`PartialScore`] per creature that is still
/// active, so a caller (e.g. NEAT-AI#3264's cascading fitness ranking) can
/// decide — without reimplementing the fused scoring loop — whether a creature
/// is already so far behind that scoring it over the rest of the corpus is
/// wasted work.
#[derive(Debug, Clone, PartialEq)]
pub struct PartialScore {
    /// Stable index of the creature within the loaded directory (sorted file
    /// order). Use this value in [`EarlyExit::AbortCreatures`] to abort it.
    pub creature_index: usize,
    /// Creature id (the `.json` file stem), matching the key in the returned
    /// [`ScoreResult`] map.
    pub key: String,
    /// Mean error over the records scored **so far** (running cost sum divided
    /// by [`records_scored`](PartialScore::records_scored)). Converges to the
    /// creature's final `error` as the sweep completes.
    pub partial_error: f64,
    /// How many training records this creature has been scored against so far.
    pub records_scored: usize,
}

/// Directive the early-exit callback returns after each scored chunk
/// (Issue #308).
///
/// The full-score path is preserved by returning [`EarlyExit::Continue`] every
/// time: with only `Continue` decisions every creature is scored over the whole
/// corpus and the results are bit-identical to [`score_from_creature_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EarlyExit {
    /// Keep scoring every still-active creature.
    Continue,
    /// Stop scoring the listed creature indices for the remainder of the
    /// corpus. Each aborted creature freezes at its current partial score,
    /// which becomes its final reported `error` (over its partial
    /// `record_count`). Unknown indices are ignored.
    AbortCreatures(Vec<usize>),
    /// Stop the corpus sweep entirely. Every still-active creature freezes at
    /// its current partial score. Use when no remaining creature is worth
    /// scoring further.
    AbortAll,
}

/// Early-exit callback trait object: invoked after each scored chunk with the
/// running per-creature [`PartialScore`]s, returning the [`EarlyExit`] directive
/// (Issue #308). Aliased to keep the internal `score_from_creature_dir_cpu`
/// signature readable (`clippy::type_complexity`).
type EarlyExitCallback<'a> = dyn FnMut(&[PartialScore]) -> EarlyExit + 'a;

// Internal sentinel error used to unwind cleanly out of `for_each_read_chunk`
// on `EarlyExit::AbortAll`. It is caught immediately after the sweep and never
// surfaced to callers — a distinct NUL-prefixed marker that no genuine I/O or
// cost error can collide with.
const EARLY_EXIT_ABORT_ALL_SENTINEL: &str = "\u{0}neat-scorer-early-exit-abort-all";

/// Score every creature in `creatures_dir` against the `.bin` training records
/// under `data_path`, returning per-creature [`ScoreResult`]s keyed by creature
/// id.
///
/// `gpu_backend` selects the scoring pipeline (CPU fallback vs. `wgpu`), and
/// `cost` is the resolved loss function dispatched through
/// [`accumulate_cost_sum`] inside the per-chunk hot loop. Returns `Err` with a
/// human-readable message on I/O, shape, or cost-resolution failure.
///
/// This is the full-score path: every creature is scored over the entire
/// corpus. For the early-exit variant see
/// [`score_from_creature_dir_with_early_exit`].
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use rust_scorer::cost::CostKind;
/// use rust_scorer::gpu::GpuBackendLabel;
/// use rust_scorer::multi_score::score_from_creature_dir;
///
/// let scores = score_from_creature_dir(
///     Path::new("creatures/"),
///     Path::new("training_data/"),
///     GpuBackendLabel::CpuFallback,
///     CostKind::Mse,
/// )
/// .unwrap();
/// for (id, result) in &scores {
///     println!("{id}: score = {}", result.score);
/// }
/// ```
pub fn score_from_creature_dir(
    creatures_dir: &Path,
    data_path: &Path,
    gpu_backend: GpuBackendLabel,
    // Issue #121 — resolved cost selector; dispatched through
    // [`accumulate_cost_sum`] inside the per-chunk hot loop.
    cost: CostKind,
) -> Result<BTreeMap<String, ScoreResult>, String> {
    // No callback registered → behaviour and scores are identical to before the
    // early-exit surface landed (Issue #308). Full-rate sampling (Issue #310).
    strict_scores(score_from_creature_dir_cpu(
        creatures_dir,
        data_path,
        gpu_backend,
        cost,
        None,
        &SampleSpec::full(),
        &mut FailureSink::strict(),
    )?)
}

/// GRQ#4387 — isolating variant of [`score_from_creature_dir_sampled`].
///
/// Scores every creature the scorer *can* score and reports the ones it cannot
/// as [`CreatureFailure`] entries keyed by the same filename stem, instead of
/// failing the whole directory on the first offender. One creature that will
/// not compile therefore costs one creature's score, not the batch's.
///
/// `Err` is still returned for genuine **run-level** faults — an unreadable
/// creature directory, a missing or empty training corpus, a misaligned
/// `.bin`, an undispatchable cost — and for the degenerate case where *no*
/// creature survived, which is a dead run rather than an isolable offender.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use rust_scorer::cost::CostKind;
/// use rust_scorer::gpu::GpuBackendLabel;
/// use rust_scorer::multi_score::score_from_creature_dir_isolated;
/// use rust_scorer::sampling::SampleSpec;
///
/// let scores = score_from_creature_dir_isolated(
///     Path::new("creatures/"),
///     Path::new("training_data/"),
///     GpuBackendLabel::CpuFallback,
///     CostKind::Mse,
///     &SampleSpec::full(),
/// )
/// .unwrap();
/// for (id, failure) in &scores.failed {
///     eprintln!("{id} was not scored: {}", failure.message);
/// }
/// ```
pub fn score_from_creature_dir_isolated(
    creatures_dir: &Path,
    data_path: &Path,
    gpu_backend: GpuBackendLabel,
    cost: CostKind,
    sample: &SampleSpec,
) -> Result<DirectoryScores, String> {
    let mut sink = FailureSink::isolating();
    let run = score_from_creature_dir_cpu(
        creatures_dir,
        data_path,
        gpu_backend,
        cost,
        None,
        sample,
        &mut sink,
    );
    match run {
        Ok(scores) => Ok(scores),
        // A run-level fault after offenders were already isolated (a missing
        // corpus, say) would otherwise swallow their names, and a fault that
        // nothing reports is the failure mode this change exists to remove.
        Err(e) if !sink.failed.is_empty() => Err(format!(
            "{e} [{} creature(s) already isolated: {}]",
            sink.failed.len(),
            sink.failed
                .iter()
                .map(|(_, failure)| failure.message.as_str())
                .collect::<Vec<_>>()
                .join("; "),
        )),
        Err(e) => Err(e),
    }
}

/// Issue #310 — record-level sub-sampling variant of
/// [`score_from_creature_dir`].
///
/// Streams the corpus once and scores every creature on the deterministic,
/// stratified subsample selected by `sample` (see [`crate::sampling`]). With
/// `sample = SampleSpec::full()` the results are identical to
/// [`score_from_creature_dir`]; with a sub-rate `sample` each creature's
/// `error` is its mean cost over the kept records and `record_count` is the
/// kept count. This is the production hot path (`rust_scorer <creatures_dir>
/// <data_dir>`) where multi-fidelity fitness delivers its wall-clock saving.
pub fn score_from_creature_dir_sampled(
    creatures_dir: &Path,
    data_path: &Path,
    gpu_backend: GpuBackendLabel,
    cost: CostKind,
    sample: &SampleSpec,
) -> Result<BTreeMap<String, ScoreResult>, String> {
    strict_scores(score_from_creature_dir_cpu(
        creatures_dir,
        data_path,
        gpu_backend,
        cost,
        None,
        sample,
        &mut FailureSink::strict(),
    )?)
}

/// Issue #308 — directory-mode batch scoring with a per-chunk early-exit hook.
///
/// Same single-pass I/O envelope and scores as [`score_from_creature_dir`], but
/// after every scored chunk `on_chunk` is called with one [`PartialScore`] per
/// still-active creature. Its [`EarlyExit`] return controls the rest of the
/// sweep:
///
/// * [`EarlyExit::Continue`] — keep scoring every active creature.
/// * [`EarlyExit::AbortCreatures`] — stop scoring the given creatures; each
///   freezes at its current partial score (its final `error` over its partial
///   `record_count`). Skipping them removes their activation cost from every
///   remaining chunk — the source of the wall-clock saving.
/// * [`EarlyExit::AbortAll`] — stop the sweep immediately; every active
///   creature freezes at its current partial score.
///
/// Returning [`EarlyExit::Continue`] on every call yields results bit-identical
/// to [`score_from_creature_dir`] (the full-score parity contract).
///
/// The callback fires once per scored chunk (a whole-record slice teased out of
/// the streamed reads), not once per record, so its overhead is amortised over
/// a large batch of records.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use rust_scorer::cost::CostKind;
/// use rust_scorer::gpu::GpuBackendLabel;
/// use rust_scorer::multi_score::{score_from_creature_dir_with_early_exit, EarlyExit};
///
/// let scores = score_from_creature_dir_with_early_exit(
///     Path::new("creatures/"),
///     Path::new("training_data/"),
///     GpuBackendLabel::CpuFallback,
///     CostKind::Mse,
///     |partials| {
///         // Abort any creature whose running error is already 10x the best.
///         let best = partials.iter().map(|p| p.partial_error).fold(f64::INFINITY, f64::min);
///         let losers: Vec<usize> = partials
///             .iter()
///             .filter(|p| p.partial_error > best * 10.0)
///             .map(|p| p.creature_index)
///             .collect();
///         if losers.is_empty() { EarlyExit::Continue } else { EarlyExit::AbortCreatures(losers) }
///     },
/// )
/// .unwrap();
/// println!("scored {} creatures", scores.len());
/// ```
pub fn score_from_creature_dir_with_early_exit<F>(
    creatures_dir: &Path,
    data_path: &Path,
    gpu_backend: GpuBackendLabel,
    cost: CostKind,
    mut on_chunk: F,
) -> Result<BTreeMap<String, ScoreResult>, String>
where
    F: FnMut(&[PartialScore]) -> EarlyExit,
{
    strict_scores(score_from_creature_dir_cpu(
        creatures_dir,
        data_path,
        gpu_backend,
        cost,
        Some(&mut on_chunk),
        &SampleSpec::full(),
        &mut FailureSink::strict(),
    )?)
}

/// Shared CPU directory-mode implementation behind [`score_from_creature_dir`]
/// (no callback) and [`score_from_creature_dir_with_early_exit`] (Issue #308).
///
/// When `on_chunk` is `None` the early-exit bookkeeping collapses to a no-op:
/// every creature stays active for the whole corpus and the numeric result is
/// identical to the pre-#308 full-score path.
fn score_from_creature_dir_cpu(
    creatures_dir: &Path,
    data_path: &Path,
    // Issue #470 vestigial-parameter sweep: every CPU-path call site (`cli.rs`,
    // the benches and the CPU legs of the parity tests) passes
    // `GpuBackendLabel::CpuFallback`, but the parameter must stay. It is read
    // into `ScoreResult::gpu_backend` — i.e. it is the value serialised as the
    // `gpuBackend` JSON field — and the label is threaded rather than hard-coded
    // so the CPU and GPU directory paths keep one shared reporting seam
    // (`score_from_creature_dir_gpu_impl` passes a real adapter label here).
    gpu_backend: GpuBackendLabel,
    cost: CostKind,
    mut on_chunk: Option<&mut EarlyExitCallback<'_>>,
    // Issue #310 — record-level sub-sampling. `SampleSpec::full()` restores the
    // pre-#310 full-corpus behaviour exactly.
    sample: &SampleSpec,
    // GRQ#4387: `strict` reproduces the pre-isolation contract byte for byte —
    // the first per-creature fault becomes the run's error, at the same point
    // in the sequence it always did. `isolating` collects them instead.
    sink: &mut FailureSink,
) -> Result<DirectoryScores, String> {
    let started = Instant::now();
    let mut loaded = load_creatures_from_dir_into(creatures_dir, sink)?;

    // GRQ#4387: isolation only has meaning while something survives to be
    // scored. With every creature rejected there is no batch left to protect,
    // so the run fails loud with the first offender's own wording — and, as
    // before Issue #571's guard was isolated, without touching the corpus.
    if loaded.is_empty() {
        return Err(first_failure_message(&sink.failed));
    }

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
    // Issue #476 — refuse a corpus whose files are not each a whole number of
    // records, before any streaming starts. The continuous-stream reader would
    // otherwise splice a record across the file boundary and shift every
    // record after it.
    assert_records_aligned(&bin_files, record_bytes)?;
    let values_per_record = num_inputs + num_outputs;

    let fused_read_target_bytes = training_read_target_bytes_from_env(record_bytes);
    let fused_read_buf_len = effective_fused_read_buf_len(record_bytes, fused_read_target_bytes);
    let training_read_backend = training_read_backend_label().to_string();
    let activation_threads = activation_worker_count_for_scorer();

    // Issue #42: compile each creature exactly once; clone the resulting
    // `CompiledNetwork` for any additional workers. `CompiledNetwork: Clone`
    // landed upstream (NEAT-AI-core#11), so a clone is functionally equivalent
    // to a second `compile_creature` call but skips JSON-graph traversal,
    // UUID resolution, and topological ordering. For 50 creatures × 16
    // workers this collapses 800 compiles into 50.
    //
    // GRQ#4387: compiling *before* the worker partition is what lets one
    // uncompilable creature be dropped — the partition is then sized to the
    // creatures that actually survived, instead of reserving worker slots for
    // a creature there is no network for.
    let compile_started = Instant::now();
    let mut templates: Vec<CompiledNetwork> = Vec::with_capacity(loaded.len());
    let mut compiled: Vec<LoadedCreature> = Vec::with_capacity(loaded.len());
    for c in loaded.drain(..) {
        compile_probe::record_compile();
        match compile_creature(&c.creature) {
            Ok(template) => {
                templates.push(template);
                compiled.push(c);
            }
            Err(e) => sink.record(
                &c.key,
                CreatureFailureKind::Compile,
                format!(
                    "Failed compiling worker network for creature '{}': {e}",
                    c.path.display()
                ),
            )?,
        }
    }
    let compile_time_secs = compile_started.elapsed().as_secs_f64();
    let loaded = compiled;
    if loaded.is_empty() {
        return Err(first_failure_message(&sink.failed));
    }

    // Issue #41: build a flat pool of worker networks sized to
    // `activation_threads`. Each chunk runs a single `par_iter_mut` over this
    // pool — no nested Rayon — so the global thread pool sees at most
    // `activation_threads` concurrent activation tasks regardless of
    // population size.
    // Issue #537: `NEAT_SCORER_WORKER_SPLIT` multiplies the task count so the
    // Rayon tail idles fewer threads; `k = 1` is the shipped default and keeps
    // the pre-#537 partition.
    let workers_per = workers_per_creature_split(
        loaded.len(),
        activation_threads,
        worker_split_factor(),
        crate::host_resources::max_worker_count(&crate::host_resources::host()),
    );
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

    // Reverse map: each worker → its owning creature index, plus that
    // creature's own `forwardOnly` flag (Issue #579). The flag is resolved
    // per worker so the hot loop indexes it with the same `worker_idx` the
    // `par_iter` already carries, instead of chasing back through the
    // creature list inside the parallel closure.
    let mut worker_creature_idx: Vec<usize> = Vec::with_capacity(total_workers);
    let mut worker_forward_only: Vec<bool> = Vec::with_capacity(total_workers);
    for (ci, &w) in workers_per.iter().enumerate() {
        for _ in 0..w {
            worker_creature_idx.push(ci);
            worker_forward_only.push(loaded[ci].creature.forward_only);
        }
    }

    // Fan each creature's single compiled template out to its worker slots.
    // The clones are the cheap half of Issue #42; the compile itself already
    // ran above, before the partition was sized.
    let mut flat_networks: Vec<CompiledNetwork> = Vec::with_capacity(total_workers);
    for (ci, template) in templates.into_iter().enumerate() {
        let workers = workers_per[ci];
        for _ in 0..workers.saturating_sub(1) {
            flat_networks.push(template.clone());
        }
        flat_networks.push(template);
    }

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
            // Issue #579: worker 0 belongs to creature 0, so probe with that
            // creature's own flag rather than a hard-coded `true`.
            worker_forward_only[0],
        )?;
    }

    let n_creatures = loaded.len();
    let mut pending: Vec<u8> = Vec::new();
    let mut head: usize = 0;
    let mut unpack_floats: Vec<f32> = Vec::new();
    let mut total_records = 0_usize;
    let mut total_mse = vec![0.0_f64; n_creatures];
    // Issue #308 — per-creature early-exit bookkeeping. `active[ci]` gates
    // whether creature `ci` is still scored; `records_scored[ci]` is how many
    // records it has been scored against (its final `record_count`). With no
    // callback every creature stays active for the whole corpus, so
    // `records_scored[ci] == total_records` and the result is unchanged.
    let mut active = vec![true; n_creatures];
    let mut records_scored = vec![0_usize; n_creatures];
    let mut abort_all = false;
    // Reused per-chunk buffers — populated inside `score_chunk`.
    let mut work_ranges: Vec<Option<Range<usize>>> = vec![None; total_workers];
    let mut worker_sums: Vec<f64> = vec![0.0; total_workers];

    let mut score_chunk = |floats: &mut Vec<f32>, n_records: usize| -> Result<(), String> {
        // Read-only consumer: borrow the unpack buffer as a slice in place.
        let floats: &[f32] = floats;
        if floats.len() != n_records * values_per_record {
            return Err("Internal float unpack length mismatch".to_string());
        }
        total_records += n_records;

        // Reset per-chunk scratch: ranges default to "no work" so workers
        // whose creature has fewer records than nominal worker count idle.
        for slot in work_ranges.iter_mut() {
            *slot = None;
        }

        // Fill work_ranges per creature. Issue #308: skip creatures the caller
        // has aborted — their worker slots stay `None`, so no activation runs
        // for them this chunk. That skipped activation is the wall-clock saving.
        for (ci, &nominal_w) in workers_per.iter().enumerate() {
            if !active[ci] {
                continue;
            }
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
                        // Issue #579: each worker scores with its own
                        // creature's flag. `false` makes the upstream kernel
                        // clear the network state before every record, so a
                        // recurrent creature's records stay independent and
                        // partitioning the chunk across workers stays valid.
                        worker_forward_only[worker_idx],
                    )?;
                } else {
                    *sum = 0.0;
                }
                Ok(())
            })?;

        // Reduce per-worker sums into per-creature totals (sequential — a few
        // hundred adds at most, so this is cheap). Worker order is fixed, so a
        // creature's partials always fold in the same order (Issue #537).
        // Inactive
        // creatures had `None` ranges → their worker sums are `0.0`, so this
        // leaves their frozen totals untouched.
        for (worker_idx, &s) in worker_sums.iter().enumerate() {
            total_mse[worker_creature_idx[worker_idx]] += s;
        }

        // Issue #308: this chunk's records count only towards creatures that
        // were actually scored (active) this chunk.
        for (ci, scored) in records_scored.iter_mut().enumerate() {
            if active[ci] {
                *scored += n_records;
            }
        }

        // Issue #308: hand the caller a running snapshot and apply its
        // early-exit decision. Skipped entirely when no callback is registered,
        // so the full-score path pays nothing.
        if let Some(cb) = on_chunk.as_deref_mut() {
            let partials: Vec<PartialScore> = (0..n_creatures)
                .filter(|&ci| active[ci])
                .map(|ci| PartialScore {
                    creature_index: ci,
                    key: loaded[ci].key.clone(),
                    // `records_scored[ci] >= n_records >= 1` here: the callback
                    // only ever sees a creature after it has scored ≥1 record.
                    partial_error: total_mse[ci] / records_scored[ci] as f64,
                    records_scored: records_scored[ci],
                })
                .collect();
            match cb(&partials) {
                EarlyExit::Continue => {}
                EarlyExit::AbortCreatures(indices) => {
                    for i in indices {
                        if i < n_creatures {
                            active[i] = false;
                        }
                    }
                }
                EarlyExit::AbortAll => {
                    for a in active.iter_mut() {
                        *a = false;
                    }
                    abort_all = true;
                    // Unwind out of `for_each_read_chunk` so the sweep stops
                    // reading immediately; caught right after the sweep.
                    return Err(EARLY_EXIT_ABORT_ALL_SENTINEL.to_string());
                }
            }
        }

        Ok(())
    };

    // Issue #3236: one sweep over the whole training corpus scores every
    // creature in the batch. Record it so `single_pass_assertion` fails loudly
    // if a future change reintroduces per-creature re-reads.
    training_pass_probe::record_sweep();
    // Issue #310: one stateful sampler threads the global record index across
    // the whole sweep so the kept subset is stable regardless of chunking.
    let mut sampler = sample.sampler();
    let sweep = for_each_read_chunk(&bin_files, fused_read_buf_len, |chunk| {
        run_io_loop(
            chunk,
            &mut pending,
            &mut head,
            &mut unpack_floats,
            record_bytes,
            &mut sampler,
            &mut score_chunk,
        )
    });
    // Issue #308: `EarlyExit::AbortAll` unwinds via a private sentinel error —
    // a clean early stop, not a failure. `for_each_read_chunk` wraps the inner
    // message with file/offset context, so match by substring (the NUL-prefixed
    // marker cannot collide with a genuine I/O or cost error). Any other error
    // is a real fault and propagates.
    match sweep {
        Ok(()) => {}
        Err(e) if abort_all && e.contains(EARLY_EXIT_ABORT_ALL_SENTINEL) => {}
        Err(e) => return Err(e),
    }

    // A mid-corpus abort legitimately leaves an unread trailing fragment, so
    // only enforce whole-record consumption when the sweep ran to completion.
    if !abort_all && head != pending.len() {
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
    for (ci, loaded_creature) in loaded.iter().enumerate() {
        let scored = records_scored[ci];
        if scored == 0 {
            // Defensive: the callback only ever sees a creature after ≥1 record,
            // so a zero here would mean an internal accounting bug — fail loud
            // rather than divide by zero (Issue #3234). Deliberately *not*
            // isolated by GRQ#4387: this is the scorer's own bookkeeping
            // breaking, not one creature being bad.
            return Err(format!(
                "Creature '{}' was scored against zero records",
                loaded_creature.path.display()
            ));
        }
        // Issue #339: shared finaliser — RMSE takes a host-side `sqrt` of the
        // mean here, reusing the MSE squared-error sum unchanged.
        let avg_error = cost.finalise_mean(total_mse[ci], scored);
        // Issue #289: the scoring API returns the typed `ScoringError`; flatten
        // it to this module's `String` contract. GRQ#4387: a creature whose own
        // numbers the score maths refuses (NaN / non-finite / negative error,
        // an impossible complexity) is that creature's fault, so it is reported
        // as an offender rather than aborting everyone else's scores.
        let scored_entry = compute_score_components(&loaded_creature.creature)
            .map_err(|e| e.to_string())
            .and_then(|components| {
                let penalty =
                    complexity_penalty(&components, GROWTH_COST).map_err(|e| e.to_string())?;
                let score = calculate_score(
                    avg_error,
                    &components,
                    GROWTH_COST,
                    loaded_creature.creature.semantic_version.as_deref(),
                )
                .map_err(|e| e.to_string())?;
                Ok((components, penalty, score))
            });
        let (components, complexity_penalty, score) = match scored_entry {
            Ok(parts) => parts,
            Err(e) => {
                sink.record(
                    &loaded_creature.key,
                    CreatureFailureKind::Score,
                    format!("Creature '{}': {e}", loaded_creature.path.display()),
                )?;
                continue;
            }
        };
        let hidden_neurons = components.hidden_neuron_count;
        let synapse_count = components.synapse_count;

        results.insert(
            loaded_creature.key.clone(),
            ScoreResult {
                score,
                error: avg_error,
                complexity_penalty,
                record_count: scored,
                hidden_neurons,
                synapse_count,
                // Issue #579: report the creature's own flag, not the batch's
                // former `forwardOnly=true` precondition.
                forward_only: loaded_creature.creature.forward_only,
                training_read_backend: training_read_backend.clone(),
                gpu_backend,
                read_buf_len: Some(fused_read_buf_len),
                // Issue #529 parallel file reads are a fused-path knob only.
                file_read_workers: None,
                activation_threads: Some(activation_threads),
                parallel_activation_batches: None,
                max_activation_batch_records: None,
                time_taken_secs: elapsed,
                compile_time_secs: Some(compile_time_secs),
                gpu_kernel: None,
                gpu_inflight_chunks: None,
                gpu_dispatch_count: None,
                cost_name: cost.as_str().to_string(),
                // Issue #310: report the effective rate only when sub-sampling
                // actually ran, so a full-corpus run keeps the pre-#310 JSON.
                sample_rate: (!sample.is_full()).then_some(sample.rate()),
            },
        );
    }

    if results.is_empty() {
        return Err(first_failure_message(&sink.failed));
    }

    Ok(DirectoryScores {
        scored: results,
        failed: std::mem::take(&mut sink.failed),
    })
}

/// Issue #180 — CPU-only pre-flight that decides whether a GPU kernel can host
/// every creature in `creatures_dir` **without creating a `wgpu` device**.
///
/// `cli.rs` calls this under `--gpu auto` *before* selecting a GPU adapter: a
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
/// * `Err(GpuPrepareError)` — the set is genuinely incompatible with the GPU
///   kernels (an unsupported squash, a shape mismatch, or an absurd neuron
///   count beyond `gpu::forward_mse_batched::MAX_NEURONS_ABSOLUTE`).
///   Issue #289 (C-GOOD-ERR): the underlying typed [`GpuPrepareError`] is now
///   returned directly instead of being flattened to a `String`, so callers can
///   `match` on the specific incompatibility. Its `Display` still yields the
///   same human-readable cause for logging.
/// * `Ok(())` — either the set is GPU-hostable, or it could not even be
///   loaded/compiled. Load/compile failures are deliberately *not* treated as
///   GPU-incompatibility: they are left for the normal scoring path to surface
///   with its existing, precise error message (e.g. shape mismatch, an invalid
///   observation width).
///
/// Issue #579: `forwardOnly` plays no part in this classification — it only
/// counts neurons and squashes. A recurrent creature is as GPU-hostable as a
/// forward-only one of the same shape, because both kernels zero every
/// non-input activation per `(creature, record)` thread.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use rust_scorer::multi_score::gpu_directory_compatible;
///
/// match gpu_directory_compatible(Path::new("creatures/")) {
///     Ok(()) => println!("set is GPU-hostable (or deferred to the scoring path)"),
///     Err(reason) => println!("must fall back to CPU: {reason}"),
/// }
/// ```
pub fn gpu_directory_compatible(creatures_dir: &Path) -> Result<(), GpuPrepareError> {
    // A load failure here (missing dir, shape mismatch, bad width, …)
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

    build_batched_network_data(&networks, num_inputs, num_outputs).map(|_| ())
}

/// CPU-only pre-flight classification of a creature directory for `--gpu auto`
/// (Issues #317, #467).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirectoryGpuProbe {
    /// Which kernel(s) the pool needs.
    pub topology: crate::gpu::forward_mse_batched::DirectoryGpuTopology,
    /// Every creature is shallow — see
    /// [`crate::gpu::forward_mse_batched::directory_pool_is_shallow`].
    pub shallow: bool,
}

/// CPU-only pre-flight probe for `--gpu auto` (Issues #317, #467).
///
/// Returns `None` when creatures cannot be loaded/compiled — defer to the
/// scoring path for the precise error. Otherwise classifies the pool so
/// [`crate::gpu::auto_should_use_gpu_directory`] can skip GPU on topologies
/// that lose to CPU on Apple Silicon production workloads, while still picking
/// GPU for the shallow scratch pools that win (Issue #467).
pub(crate) fn gpu_directory_probe_for_dir(creatures_dir: &Path) -> Option<DirectoryGpuProbe> {
    let loaded = load_creatures_from_dir(creatures_dir).ok()?;
    if loaded.is_empty() {
        return None;
    }
    let mut networks: Vec<CompiledNetwork> = Vec::with_capacity(loaded.len());
    for c in &loaded {
        networks.push(compile_creature(&c.creature).ok()?);
    }
    Some(DirectoryGpuProbe {
        topology: crate::gpu::forward_mse_batched::directory_gpu_topology(&networks),
        shallow: crate::gpu::forward_mse_batched::directory_pool_is_shallow(&networks),
    })
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
///   accepted but capped to `2` so device memory stays bounded. Scratch/mixed
///   pools are forced to `1` — pipelining those kernels at production-scale read
///   sizes can SIGSEGV on Metal across `.bin` file boundaries (Issue #319).
///
/// Returns the same per-creature `BTreeMap<String, ScoreResult>` as the CPU
/// path, with `gpuKernel` (`forward_mse_batched` for creatures within the 256
/// cap, `forward_mse_scratch` above it — Issue #182), `gpuInflightChunks`, and
/// `gpuDispatchCount` populated for every entry.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use std::sync::Arc;
/// use rust_scorer::cost::CostKind;
/// use rust_scorer::gpu::{GpuBackendLabel, select_adapter};
/// use rust_scorer::multi_score::score_from_creature_dir_gpu;
///
/// // A real adapter is required — this only runs on a GPU-capable host.
/// let ctx = Arc::new(select_adapter().unwrap().expect("a compatible GPU adapter"));
/// let scores = score_from_creature_dir_gpu(
///     Path::new("creatures/"),
///     Path::new("training_data/"),
///     GpuBackendLabel::Metal,
///     ctx,
///     2, // pipelined dispatches
///     CostKind::Mse,
/// )
/// .unwrap();
/// println!("scored {} creatures on the GPU", scores.len());
/// ```
pub fn score_from_creature_dir_gpu(
    creatures_dir: &Path,
    data_path: &Path,
    gpu_backend: GpuBackendLabel,
    ctx: Arc<GpuContext>,
    inflight_chunks: usize,
    // Issue #121/#316 — the batched/scratch GPU kernels host MSE, RMSE and MAE
    // (squared vs absolute per-record error, selected by `Header.cost_kind`).
    // Any other cost is a hard error here; callers under `--gpu auto` are
    // expected to detect this via [`crate::gpu::auto_should_use_gpu`] and
    // route to the CPU path instead.
    cost: CostKind,
) -> Result<BTreeMap<String, ScoreResult>, String> {
    // Full-rate (Issue #310) — identical to the pre-sampling GPU path.
    score_from_creature_dir_gpu_impl(
        creatures_dir,
        data_path,
        gpu_backend,
        ctx,
        inflight_chunks,
        cost,
        &SampleSpec::full(),
    )
}

/// Issue #310 — record-level sub-sampling variant of
/// [`score_from_creature_dir_gpu`].
///
/// Same single-pass GPU envelope, but only the deterministic subsample selected
/// by `sample` is dispatched to the kernel. `SampleSpec::full()` reproduces the
/// full-corpus GPU result exactly.
pub fn score_from_creature_dir_gpu_sampled(
    creatures_dir: &Path,
    data_path: &Path,
    gpu_backend: GpuBackendLabel,
    ctx: Arc<GpuContext>,
    inflight_chunks: usize,
    cost: CostKind,
    sample: &SampleSpec,
) -> Result<BTreeMap<String, ScoreResult>, String> {
    score_from_creature_dir_gpu_impl(
        creatures_dir,
        data_path,
        gpu_backend,
        ctx,
        inflight_chunks,
        cost,
        sample,
    )
}

#[allow(clippy::too_many_arguments)]
fn score_from_creature_dir_gpu_impl(
    creatures_dir: &Path,
    data_path: &Path,
    gpu_backend: GpuBackendLabel,
    ctx: Arc<GpuContext>,
    inflight_chunks: usize,
    cost: CostKind,
    sample: &SampleSpec,
) -> Result<BTreeMap<String, ScoreResult>, String> {
    if !cost.gpu_supported() {
        return Err(format!(
            "GPU kernel not implemented for cost {}: the batched/scratch kernels host MSE, RMSE \
             and MAE only (use --gpu auto for silent CPU fallback, or --gpu off to skip GPU entirely)",
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
    // Issue #476 — refuse a corpus whose files are not each a whole number of
    // records, before any streaming starts. The continuous-stream reader would
    // otherwise splice a record across the file boundary and shift every
    // record after it.
    assert_records_aligned(&bin_files, record_bytes)?;
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
        compile_probe::record_compile();
        let net = compile_creature(&c.creature).map_err(|e| {
            format!(
                "Failed compiling network for creature '{}': {e}",
                c.path.display()
            )
        })?;
        networks.push(net);
    }
    let compile_time_secs = compile_started.elapsed().as_secs_f64();

    // Build the GPU runner(s) up-front; if any creature is incompatible with the
    // shader (unsupported squash, too many neurons, shape mismatch) we fall
    // back to the CPU path so callers still get a result. Mixed pools route
    // small creatures to the private kernel and large ones to scratch (#317).
    let mut runners =
        match DirectoryGpuRunners::new(ctx.clone(), &networks, num_inputs, num_outputs, cost) {
            Ok(r) => r,
            Err(e) => {
                return Err(format!(
                    "GPU runner cannot host this creature set ({e}); rerun with --gpu off"
                ));
            }
        };

    let inflight_chunks =
        effective_directory_gpu_inflight(runners.topology(), inflight_chunks).clamp(1, 2);
    let mut total_records = 0_usize;
    let mut total_mse = vec![0.0_f64; loaded.len()];
    let mut pending: Vec<u8> = Vec::new();
    let mut head: usize = 0;
    let mut unpack_floats: Vec<f32> = Vec::new();
    // Issue #310: one sampler for the whole sweep, shared by whichever branch
    // (synchronous or pipelined) runs below.
    let mut sampler = sample.sampler();

    if inflight_chunks <= 1 {
        // Synchronous: one chunk in flight at a time.
        let mut score_chunk = |floats: &mut Vec<f32>, n_records: usize| -> Result<(), String> {
            // Synchronous path borrows the unpack buffer in place — no handoff.
            let floats: &[f32] = floats;
            if floats.len() != n_records * values_per_record {
                return Err("Internal float unpack length mismatch".to_string());
            }
            total_records += n_records;
            let sums = runners.score_chunk(floats, n_records)?;
            for (i, s) in sums.iter().enumerate() {
                total_mse[i] += s;
            }
            Ok(())
        };

        // Issue #3236: single sweep over the corpus for the whole GPU batch.
        training_pass_probe::record_sweep();
        for_each_read_chunk(&bin_files, fused_read_buf_len, |chunk| {
            run_io_loop(
                chunk,
                &mut pending,
                &mut head,
                &mut unpack_floats,
                record_bytes,
                &mut sampler,
                &mut score_chunk,
            )
        })?;
    } else {
        // Pipelined: a worker thread runs GPU dispatches while the I/O thread
        // continues unpacking. Bounded crossbeam-style channel of capacity
        // `inflight_chunks` keeps memory bounded under streaming load.
        use std::sync::mpsc;
        use std::thread;

        // Move runners into worker thread; results come back through `result_rx`.
        // Consumed unpack buffers return through `recycle_rx` so the I/O thread
        // can reuse them instead of cloning each chunk (Issue #202).
        let n_creatures = loaded.len();
        let (work_tx, work_rx) = mpsc::sync_channel::<Option<(Vec<f32>, usize)>>(inflight_chunks);
        let (result_tx, result_rx) = mpsc::channel::<Result<(usize, Vec<f64>), String>>();
        let (recycle_tx, recycle_rx) = mpsc::channel::<Vec<f32>>();

        let worker = thread::spawn(move || {
            while let Ok(Some((floats, n_records))) = work_rx.recv() {
                if floats.len() != n_records * values_per_record {
                    let _ =
                        result_tx.send(Err("Internal float unpack length mismatch".to_string()));
                    return runners;
                }
                let sums = match runners.score_chunk(&floats, n_records) {
                    Ok(sums) => sums,
                    Err(e) => {
                        // Readback failed (e.g. device loss). Surface the error
                        // so the `--gpu auto` caller can fall back to the CPU
                        // instead of panicking mid-run (Issue #273).
                        let _ = result_tx.send(Err(e));
                        return runners;
                    }
                };
                if result_tx.send(Ok((n_records, sums))).is_err() {
                    break;
                }
                // Hand the consumed buffer back for reuse. A send error just
                // means the I/O thread has gone away, so drop the buffer.
                let _ = recycle_tx.send(floats);
            }
            runners
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

        let mut pool = FloatBufPool::new();
        let mut submit_chunk = |floats: &mut Vec<f32>, n_records: usize| -> Result<(), String> {
            // Cap in-flight so the channel never queues more than `inflight_chunks`
            // chunks. When already at the cap, drain one result first.
            while pending_results >= inflight_chunks {
                drain_one(&mut total_records, &mut total_mse, &mut pending_results)?;
            }
            // Reclaim any buffers the worker has finished with.
            while let Ok(buf) = recycle_rx.try_recv() {
                pool.recycle(buf);
            }
            // Transfer ownership of the freshly-unpacked buffer to the worker and
            // swap a recycled (or fresh) buffer into the unpack slot. No per-chunk
            // clone — the I/O thread keeps streaming into the new buffer while the
            // worker scores the old one (Issue #202).
            let outgoing = std::mem::replace(floats, pool.take());
            work_tx
                .send(Some((outgoing, n_records)))
                .map_err(|_| "GPU worker hung up before chunk submission".to_string())?;
            pending_results += 1;
            Ok(())
        };

        // Issue #3236: single sweep over the corpus for the whole pipelined
        // GPU batch.
        training_pass_probe::record_sweep();
        for_each_read_chunk(&bin_files, fused_read_buf_len, |chunk| {
            run_io_loop(
                chunk,
                &mut pending,
                &mut head,
                &mut unpack_floats,
                record_bytes,
                &mut sampler,
                &mut submit_chunk,
            )
        })?;

        // Drain remaining results before tearing down the worker.
        while pending_results > 0 {
            drain_one(&mut total_records, &mut total_mse, &mut pending_results)?;
        }
        let _ = work_tx.send(None);
        let recovered_runners = worker.join().map_err(|_| "GPU worker thread panicked")?;
        // Move dispatch_count back so we can report it in the JSON.
        runners = recovered_runners;
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
    let dispatch_count = runners.dispatch_count();
    let gpu_kernel_label = runners.kernel_label();
    let mut results = BTreeMap::new();
    for (loaded_creature, mse_sum) in loaded.iter().zip(total_mse.iter()) {
        // Issue #339/#316: the GPU creature-directory finalisation. `mse_sum` is
        // the per-record error sum the kernel returns — squared error for
        // MSE/RMSE, absolute error for MAE (`Header.cost_kind`). Routing it
        // through the shared finaliser gives RMSE its host-side `sqrt` on the GPU
        // path too (no new kernel) and reports the plain mean for MSE/MAE,
        // matching the CPU path within tolerance.
        let avg_error = cost.finalise_mean(*mse_sum, total_records);
        // Issue #289: the scoring API now returns the typed `ScoringError`;
        // flatten to this module's `String` error contract at the boundary.
        let components =
            compute_score_components(&loaded_creature.creature).map_err(|e| e.to_string())?;
        let hidden_neurons = components.hidden_neuron_count;
        let synapse_count = components.synapse_count;

        let complexity_penalty =
            complexity_penalty(&components, GROWTH_COST).map_err(|e| e.to_string())?;

        let score = calculate_score(
            avg_error,
            &components,
            GROWTH_COST,
            loaded_creature.creature.semantic_version.as_deref(),
        )
        .map_err(|e| e.to_string())?;

        results.insert(
            loaded_creature.key.clone(),
            ScoreResult {
                score,
                error: avg_error,
                complexity_penalty,
                record_count: total_records,
                hidden_neurons,
                synapse_count,
                // Issue #579: report the creature's own flag. The kernels zero
                // every non-input activation per `(creature, record)` thread,
                // so a GPU thread never carries state between records — the
                // reset semantics `forwardOnly=false` requires hold here
                // without a shader change.
                forward_only: loaded_creature.creature.forward_only,
                training_read_backend: training_read_backend.clone(),
                gpu_backend,
                read_buf_len: Some(fused_read_buf_len),
                // Issue #529 parallel file reads are a fused-path knob only.
                file_read_workers: None,
                activation_threads: Some(activation_threads),
                parallel_activation_batches: None,
                max_activation_batch_records: None,
                time_taken_secs: elapsed,
                compile_time_secs: Some(compile_time_secs),
                gpu_kernel: Some(gpu_kernel_label.clone()),
                gpu_inflight_chunks: Some(inflight_chunks),
                gpu_dispatch_count: Some(dispatch_count),
                cost_name: cost.as_str().to_string(),
                // Issue #310: report the effective rate only when sub-sampling
                // actually ran, so a full-corpus run keeps the pre-#310 JSON.
                sample_rate: (!sample.is_full()).then_some(sample.rate()),
            },
        );
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #571: a directory holding a creature with `input: 0` or
    /// `output: 0` fails the load with the shared wording, naming the file,
    /// before the training-data path is touched (it does not exist here).
    #[test]
    fn directory_load_rejects_zero_input_and_output_before_reading_data() {
        use crate::fixture_json::{creature_envelope, neuron_json, synapse_json};
        let neurons = vec![neuron_json("output", "output-0", 0.0, "IDENTITY")];
        let synapses = vec![synapse_json("input-0", "output-0", 1.0)];
        for (inputs, outputs, expected) in [
            (0, 1, "Must have at least one input neurons was: 0"),
            (1, 0, "Must have at least one output neurons was: 0"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let creatures = tmp.path().join("creatures");
            fs::create_dir(&creatures).unwrap();
            fs::write(
                creatures.join("ok.json"),
                creature_envelope(1, 1, &neurons, &synapses),
            )
            .unwrap();
            fs::write(
                creatures.join("widthless.json"),
                creature_envelope(inputs, outputs, &neurons, &synapses),
            )
            .unwrap();
            let missing_data = tmp.path().join("no-such-data-dir");

            let err = score_from_creature_dir_sampled(
                &creatures,
                &missing_data,
                GpuBackendLabel::CpuFallback,
                CostKind::Mse,
                &SampleSpec::full(),
            )
            .expect_err("widthless creature must fail the directory load");
            assert!(err.contains(expected), "expected `{expected}`, got `{err}`");
            assert!(
                err.contains("widthless.json"),
                "error must name the offending file, got `{err}`"
            );
            assert!(
                !err.contains("is not a directory"),
                "guard must fire before the data path is read, got `{err}`"
            );
        }
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

    /// Issue #537: `split = 1` must reproduce the pre-#537 partition for every
    /// population/thread combination, so the shipped default is unchanged.
    #[test]
    fn workers_per_creature_split_one_matches_legacy_partition() {
        for n in 1..=64_usize {
            for threads in [1_usize, 2, 8, 10, 12, 16, 64] {
                assert_eq!(
                    workers_per_creature_split(n, threads, 1, usize::MAX),
                    workers_per_creature(n, threads),
                    "split=1 must match the legacy partition at n={n}, threads={threads}"
                );
            }
        }
    }

    /// Issue #537: at the production population (N=50 on 10 threads) the legacy
    /// partition hands the pool 50 indivisible tasks. A `split` of `k` must
    /// hand it `k × 50` finer tasks instead.
    #[test]
    fn workers_per_creature_split_multiplies_task_count_above_thread_count() {
        for k in [2_usize, 4, 8] {
            let w = workers_per_creature_split(50, 10, k, usize::MAX);
            assert_eq!(w.len(), 50);
            assert_eq!(
                w.iter().sum::<usize>(),
                50 * k,
                "split={k} must multiply the 50-worker budget"
            );
            assert!(
                w.iter().all(|&x| x == k),
                "split={k} must divide evenly across an exact-multiple population, got {w:?}"
            );
        }
    }

    /// Issue #537: below the thread count the `activation_threads` budget is
    /// the base, so `split` multiplies that instead of the population size.
    #[test]
    fn workers_per_creature_split_multiplies_thread_budget_below_population() {
        // 3 creatures, 8 threads, k=2 → 16 workers → base=5, rem=1.
        let w = workers_per_creature_split(3, 8, 2, usize::MAX);
        assert_eq!(w, vec![6, 5, 5]);
        assert_eq!(w.iter().sum::<usize>(), 16);
    }

    /// Issue #537: the total worker count must honour the host RAM ceiling —
    /// one `CompiledNetwork` clone is held per worker.
    #[test]
    fn workers_per_creature_split_clamps_to_host_worker_ceiling() {
        let w = workers_per_creature_split(50, 10, 8, 256);
        assert_eq!(
            w.iter().sum::<usize>(),
            256,
            "400 requested workers must clamp to the 256 ceiling, got {w:?}"
        );
        assert!(w.iter().all(|&x| x >= 1), "every creature keeps a worker");
    }

    /// Issue #537: a ceiling below the population cannot starve a creature —
    /// one worker per creature is the irreducible minimum.
    #[test]
    fn workers_per_creature_split_never_drops_below_one_per_creature() {
        let w = workers_per_creature_split(50, 10, 4, 4);
        assert_eq!(w, vec![1; 50]);
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
