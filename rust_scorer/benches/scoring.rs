//! Criterion benchmarks for the `rust_scorer` hot paths — Issue #36.
//!
//! Three groups, all built on a shared lazily-created tempdir fixture so the
//! synthetic creature(s) and `.bin` corpus are paid for **once per process**:
//!
//! * `score_from_json_fused` — forward-only fused path, exercised through
//!   [`rust_scorer::stream_score::accumulate_cost_sum_forward_only_fused`] (the
//!   same hot path the CLI runs in default mode).
//! * `score_from_creature_dir` — directory mode at `N=10` and `N=50` creatures,
//!   exercised through [`rust_scorer::multi_score::score_from_creature_dir`].
//! * `unpack_and_mse_inner` — micro-benchmark over a fixed in-memory chunk
//!   that mirrors the inner loop combining little-endian `f32` unpack and
//!   `mse_sum_batch_packed`.
//!
//! ## Sizing
//!
//! The training corpus size and creature shape are parameterised via
//! environment variables so contributors can sweep 50–200 MB without editing
//! the bench. Defaults are conservative (16 MB) to keep `cargo bench` runtime
//! reasonable on CI/dev machines; the realistic perf target is `BENCH_SCORING_BYTES=200000000`.
//!
//! | Variable | Default | Purpose |
//! |---|---|---|
//! | `BENCH_SCORING_BYTES` | `16777216` (16 MiB) | total bytes per `.bin` corpus |
//! | `BENCH_SCORING_INPUTS` | `8` | inputs per record |
//! | `BENCH_SCORING_OUTPUTS` | `2` | outputs per record |
//! | `BENCH_SCORING_HIDDEN` | `8` | hidden neurons per synthetic creature |
//!
//! Reproduce on your host with `./scripts/run-benches.sh` or
//! `cargo bench -p rust_scorer`. Update `docs/performance-baseline.md` with
//! the median and standard deviation when establishing a new baseline.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use neat_core::creature::{compile_creature, parse_creature_json};
use neat_core::loss::mse_sum_batch_packed;
use neat_core::training_data::{TrainingDataConfig, find_bin_files};

use std::sync::Arc;

use rust_scorer::cost::CostKind;
use rust_scorer::fixture_json::{creature_envelope, neuron_json, synapse_json};
use rust_scorer::gpu::{GpuBackendLabel, GpuMode, resolve_backend, select_adapter};
use rust_scorer::multi_score::{
    EarlyExit, score_from_creature_dir, score_from_creature_dir_gpu,
    score_from_creature_dir_with_early_exit,
};
use rust_scorer::prod_fixture::{
    corpus_record_count, load_production_creature, production_creature_path_from_env,
};
use rust_scorer::stream_score::accumulate_cost_sum_forward_only_fused;

use tempfile::TempDir;

const DEFAULT_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_NUM_INPUTS: usize = 8;
const DEFAULT_NUM_OUTPUTS: usize = 2;
const DEFAULT_HIDDEN: usize = 8;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn bench_total_bytes() -> usize {
    env_usize("BENCH_SCORING_BYTES", DEFAULT_TOTAL_BYTES)
}

fn bench_num_inputs() -> usize {
    env_usize("BENCH_SCORING_INPUTS", DEFAULT_NUM_INPUTS).max(1)
}

fn bench_num_outputs() -> usize {
    env_usize("BENCH_SCORING_OUTPUTS", DEFAULT_NUM_OUTPUTS).max(1)
}

fn bench_hidden() -> usize {
    env_usize("BENCH_SCORING_HIDDEN", DEFAULT_HIDDEN)
}

/// Representative production squash mix used when
/// `BENCH_SCORING_HIDDEN_SQUASH=MIXED` — the GPU blockers from Scorer#299 that
/// Issue #305 unblocked, plus the base set. Names match `parse_squash_name`.
const MIXED_SQUASHES: &[&str] = &[
    "TANH",
    "LOGISTIC",
    "RELU",
    "GELU",
    "SELU",
    "SINE",
    "ABSOLUTE",
    "BENT_IDENTITY",
    "Cube",
    "HARD_TANH",
];

/// Hidden-layer squash for the synthetic creature (Issue #305).
///
/// `BENCH_SCORING_HIDDEN_SQUASH` selects the activation so the GPU-vs-CPU
/// directory A/B can exercise the production squash set the shader now hosts.
/// The default `TANH` preserves historical baselines; `MIXED` cycles
/// [`MIXED_SQUASHES`] across the hidden neurons to mimic a real production creature.
fn bench_hidden_squash(h: usize) -> String {
    match std::env::var("BENCH_SCORING_HIDDEN_SQUASH").ok().as_deref() {
        None | Some("") | Some("TANH") => "TANH".to_string(),
        Some("MIXED") => MIXED_SQUASHES[h % MIXED_SQUASHES.len()].to_string(),
        // A literal squash name applied to every hidden neuron. An unknown name
        // fails the creature parse loudly downstream, which is the intent.
        Some(name) => name.to_string(),
    }
}

/// Build a forward-only synthetic creature JSON wired as a small dense MLP.
fn synthetic_creature_json(num_inputs: usize, num_outputs: usize, hidden: usize) -> String {
    let mut neurons: Vec<String> = Vec::with_capacity(hidden + num_outputs);
    for h in 0..hidden {
        let squash = bench_hidden_squash(h);
        neurons.push(neuron_json("hidden", &format!("hidden-{h}"), 0.05, &squash));
    }
    for o in 0..num_outputs {
        neurons.push(neuron_json(
            "output",
            &format!("output-{o}"),
            0.0,
            "IDENTITY",
        ));
    }

    let mut synapses: Vec<String> = Vec::with_capacity(num_inputs * hidden + hidden * num_outputs);
    // Input -> hidden
    for i in 0..num_inputs {
        for h in 0..hidden {
            // Vary weight slightly so activations are non-degenerate.
            let w = 0.05 + 0.001 * ((i * hidden + h) as f64);
            synapses.push(synapse_json(
                &format!("input-{i}"),
                &format!("hidden-{h}"),
                w,
            ));
        }
    }
    // Hidden -> output
    for h in 0..hidden {
        for o in 0..num_outputs {
            let w = 0.1 + 0.001 * ((h * num_outputs + o) as f64);
            synapses.push(synapse_json(
                &format!("hidden-{h}"),
                &format!("output-{o}"),
                w,
            ));
        }
    }

    creature_envelope(num_inputs, num_outputs, &neurons, &synapses)
}

/// Write a single `0.bin` file holding `total_bytes` worth of synthetic packed records.
fn write_synthetic_bin(data_dir: &Path, num_inputs: usize, num_outputs: usize, total_bytes: usize) {
    let values_per_record = num_inputs + num_outputs;
    let record_bytes = values_per_record * std::mem::size_of::<f32>();
    let n_records = (total_bytes / record_bytes).max(1);

    let path = data_dir.join("0.bin");
    let f = File::create(&path).expect("create training bin file");
    let mut w = BufWriter::with_capacity(1 << 20, f);
    let mut bytes = Vec::with_capacity(record_bytes);
    for i in 0..n_records {
        bytes.clear();
        for k in 0..values_per_record {
            // Deterministic but record-varying values keep the network out of a saturated
            // regime (vs all-zeroes) so the bench reflects realistic activation work.
            let v = ((i.wrapping_mul(31) + k) as f32 * 1.0e-3).sin();
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        w.write_all(&bytes).expect("write training bytes");
    }
    w.flush().expect("flush bin file");
}

struct Fixture {
    _tmp: TempDir,
    creature_path: PathBuf,
    creatures_root: PathBuf,
    data_dir: PathBuf,
    num_inputs: usize,
    num_outputs: usize,
    total_bytes: usize,
}

fn fixture() -> &'static Fixture {
    static FIX: OnceLock<Fixture> = OnceLock::new();
    FIX.get_or_init(|| {
        let tmp = TempDir::new().expect("tempdir");
        let creatures_root = tmp.path().join("creatures-root");
        fs::create_dir_all(&creatures_root).unwrap();
        let data_dir = tmp.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();

        let num_inputs = bench_num_inputs();
        let num_outputs = bench_num_outputs();
        let total_bytes = bench_total_bytes();
        let json = synthetic_creature_json(num_inputs, num_outputs, bench_hidden());

        // Single-creature path: top-level synthetic.json.
        let creature_path = tmp.path().join("synthetic.json");
        fs::write(&creature_path, &json).unwrap();

        // Multi-creature root with a fixed pool of identical creatures; per-N
        // sub-directories are materialised lazily by the directory benchmark.
        let pool_size = 200;
        for n in 0..pool_size {
            fs::write(creatures_root.join(format!("creature-{n:03}.json")), &json).unwrap();
        }

        write_synthetic_bin(&data_dir, num_inputs, num_outputs, total_bytes);

        Fixture {
            _tmp: tmp,
            creature_path,
            creatures_root,
            data_dir,
            num_inputs,
            num_outputs,
            total_bytes,
        }
    })
}

fn bench_score_from_json_fused(c: &mut Criterion) {
    let fix = fixture();
    let json = fs::read_to_string(&fix.creature_path).expect("read creature");
    let creature = parse_creature_json(&json).expect("parse creature");
    let bin_files = find_bin_files(&fix.data_dir).expect("find bin files");
    let config = TrainingDataConfig {
        num_inputs: fix.num_inputs,
        num_outputs: fix.num_outputs,
    };

    let mut group = c.benchmark_group("score_from_json_fused");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(8));
    group.throughput(Throughput::Bytes(fix.total_bytes as u64));
    group.bench_function("forward_only", |b| {
        b.iter_batched(
            || compile_creature(&creature).expect("compile"),
            |mut net| {
                let r = accumulate_cost_sum_forward_only_fused(
                    CostKind::Mse,
                    &bin_files,
                    &config,
                    &mut net,
                )
                .expect("fused MSE accumulate");
                black_box(r);
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

fn bench_score_from_creature_dir(c: &mut Criterion) {
    let fix = fixture();
    let mut group = c.benchmark_group("score_from_creature_dir");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));
    group.throughput(Throughput::Bytes(fix.total_bytes as u64));

    // Issue #42: directory-mode start-up was dominated by per-worker
    // `compile_creature` calls (population × `activation_threads`). Sweep the
    // same population sizes we benchmarked in #36 so the before/after
    // numbers stay comparable.
    for &n in &[1_usize, 10_usize, 50_usize, 200_usize] {
        // Materialise an N-creature sub-directory by copying from the pool.
        let sub_dir = fix
            .creatures_root
            .parent()
            .unwrap()
            .join(format!("dir-{n}"));
        if !sub_dir.exists() {
            fs::create_dir_all(&sub_dir).unwrap();
            for i in 0..n {
                let src = fix.creatures_root.join(format!("creature-{i:03}.json"));
                let dst = sub_dir.join(format!("creature-{i:03}.json"));
                fs::copy(&src, &dst).expect("copy creature");
            }
        }

        group.bench_with_input(BenchmarkId::new("creatures", n), &sub_dir, |b, dir| {
            b.iter(|| {
                let result = score_from_creature_dir(
                    dir,
                    &fix.data_dir,
                    GpuBackendLabel::CpuFallback,
                    CostKind::default(),
                )
                .expect("multi-creature score");
                black_box(result);
            });
        });
    }
    group.finish();
}

/// Local copy of the production unpack inner loop. Kept in this bench file so
/// the micro-bench does not require widening the crate's public surface; the
/// implementation matches `multi_score::unpack_f32s_le` and
/// `stream_score::unpack_f32s_le` byte-for-byte (single-source-of-truth lives
/// in those modules; this duplicate is documented and exists only inside the
/// bench harness).
fn unpack_f32s_le_bench(src: &[u8], dst: &mut Vec<f32>, n: usize) {
    debug_assert_eq!(src.len(), n * 4);
    dst.clear();
    if dst.capacity() < n {
        dst.reserve(n - dst.capacity());
    }

    #[cfg(target_endian = "little")]
    {
        // SAFETY: `src.len() == n * 4`, capacity ≥ `n` after the reserve above;
        // every element [0, n) is initialised before `set_len(n)`.
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

fn bench_unpack_and_mse_inner(c: &mut Criterion) {
    let num_inputs = bench_num_inputs();
    let num_outputs = bench_num_outputs();
    let values_per_record = num_inputs + num_outputs;
    // Fixed in-memory chunk: 16K records — fits in L2/L3 on most machines and
    // exercises the inner loop without re-paying I/O setup costs.
    let n_records = 16 * 1024;
    let total_floats = n_records * values_per_record;
    let total_bytes = total_floats * std::mem::size_of::<f32>();

    let mut bytes = Vec::with_capacity(total_bytes);
    for i in 0..total_floats {
        let v = (i as f32 * 1.0e-3).sin();
        bytes.extend_from_slice(&v.to_le_bytes());
    }

    let json = synthetic_creature_json(num_inputs, num_outputs, bench_hidden());
    let creature = parse_creature_json(&json).expect("parse creature");

    let mut group = c.benchmark_group("unpack_and_mse_inner");
    group.throughput(Throughput::Bytes(total_bytes as u64));
    group.bench_function("unpack_then_mse", |b| {
        let mut floats: Vec<f32> = Vec::with_capacity(total_floats);
        let mut net = compile_creature(&creature).expect("compile");
        b.iter(|| {
            unpack_f32s_le_bench(&bytes, &mut floats, total_floats);
            let s = mse_sum_batch_packed(&mut net, &floats, num_inputs, num_outputs, true);
            black_box(s);
        });
    });
    group.finish();
}

/// Issue #82: GPU multi-creature batched dispatch bench. Mirrors the CPU
/// `score_from_creature_dir` group at N=10 and N=50 so before/after numbers
/// stay directly comparable. When no GPU adapter is available the bench
/// silently no-ops (the function still records a single sample so Criterion
/// produces a row in the report), letting CPU-only CI runners pass.
fn bench_gpu_score_from_creature_dir(c: &mut Criterion) {
    let fix = fixture();
    let mut group = c.benchmark_group("gpu_score_from_creature_dir");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));
    group.throughput(Throughput::Bytes(fix.total_bytes as u64));

    // Skip cleanly when no GPU is available.
    let backend = resolve_backend(GpuMode::Auto).unwrap_or(GpuBackendLabel::CpuFallback);
    if backend == GpuBackendLabel::CpuFallback {
        eprintln!("gpu_score_from_creature_dir: no GPU adapter — skipping");
        group.finish();
        return;
    }
    let ctx = match select_adapter() {
        Ok(Some(c)) => Arc::new(c),
        _ => {
            eprintln!("gpu_score_from_creature_dir: select_adapter returned no context");
            group.finish();
            return;
        }
    };

    for &n in &[10_usize, 50_usize] {
        let sub_dir = fix
            .creatures_root
            .parent()
            .unwrap()
            .join(format!("dir-{n}"));
        if !sub_dir.exists() {
            fs::create_dir_all(&sub_dir).unwrap();
            for i in 0..n {
                let src = fix.creatures_root.join(format!("creature-{i:03}.json"));
                let dst = sub_dir.join(format!("creature-{i:03}.json"));
                fs::copy(&src, &dst).expect("copy creature");
            }
        }

        group.bench_with_input(BenchmarkId::new("creatures", n), &sub_dir, |b, dir| {
            let ctx = ctx.clone();
            b.iter(|| {
                let result = score_from_creature_dir_gpu(
                    dir,
                    &fix.data_dir,
                    backend,
                    ctx.clone(),
                    2,
                    CostKind::default(),
                )
                .expect("gpu multi-creature score");
                black_box(result);
            });
        });
    }
    group.finish();
}

/// Issue #82: pipelining toggle — same N=50 dataset, run once with
/// `inflight_chunks=1` (synchronous) and once with `inflight_chunks=2`
/// (double-buffered). The acceptance criterion is "pipelined ≥ non-pipelined".
fn bench_gpu_pipelining_toggle(c: &mut Criterion) {
    let fix = fixture();
    let mut group = c.benchmark_group("gpu_pipelining_toggle");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));
    group.throughput(Throughput::Bytes(fix.total_bytes as u64));

    let backend = resolve_backend(GpuMode::Auto).unwrap_or(GpuBackendLabel::CpuFallback);
    if backend == GpuBackendLabel::CpuFallback {
        eprintln!("gpu_pipelining_toggle: no GPU adapter — skipping");
        group.finish();
        return;
    }
    let ctx = match select_adapter() {
        Ok(Some(c)) => Arc::new(c),
        _ => {
            eprintln!("gpu_pipelining_toggle: select_adapter returned no context");
            group.finish();
            return;
        }
    };

    let n = 50_usize;
    let sub_dir = fix
        .creatures_root
        .parent()
        .unwrap()
        .join(format!("dir-{n}"));
    if !sub_dir.exists() {
        fs::create_dir_all(&sub_dir).unwrap();
        for i in 0..n {
            let src = fix.creatures_root.join(format!("creature-{i:03}.json"));
            let dst = sub_dir.join(format!("creature-{i:03}.json"));
            fs::copy(&src, &dst).expect("copy creature");
        }
    }

    for &inflight in &[1_usize, 2_usize] {
        group.bench_with_input(
            BenchmarkId::new("inflight", inflight),
            &(&sub_dir, inflight),
            |b, (dir, inflight)| {
                let ctx = ctx.clone();
                let inflight = *inflight;
                b.iter(|| {
                    let result = score_from_creature_dir_gpu(
                        dir,
                        &fix.data_dir,
                        backend,
                        ctx.clone(),
                        inflight,
                        CostKind::default(),
                    )
                    .expect("gpu multi-creature score");
                    black_box(result);
                });
            },
        );
    }
    group.finish();
}

/// Issue #182: large-creature CPU vs GPU comparison. Real evolved creatures
/// routinely exceed the old 256-neuron shader cap (observed 4139); this group
/// scores creatures well above it on both paths so the before (CPU+PGO) and
/// after (`forward_mse_scratch` GPU kernel) numbers are directly comparable.
///
/// Sized via `BENCH_LARGE_HIDDEN` (default 4000 hidden neurons → ~4010 total),
/// `BENCH_LARGE_CREATURES` (default 8), and `BENCH_LARGE_BYTES` (default 32 MiB)
/// to keep the fixture build and runtime bounded. When no GPU adapter is
/// present the GPU row is skipped so CPU-only CI runners still pass.
fn bench_large_creature_cpu_vs_gpu(c: &mut Criterion) {
    let hidden = env_usize("BENCH_LARGE_HIDDEN", 4000).max(257);
    let n_creatures = env_usize("BENCH_LARGE_CREATURES", 8).max(1);
    let total_bytes = env_usize("BENCH_LARGE_BYTES", 32 * 1024 * 1024);
    let num_inputs = bench_num_inputs();
    let num_outputs = bench_num_outputs();

    let tmp = TempDir::new().expect("tempdir");
    let creatures_dir = tmp.path().join("creatures");
    fs::create_dir_all(&creatures_dir).unwrap();
    let data_dir = tmp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();

    let json = synthetic_creature_json(num_inputs, num_outputs, hidden);
    for n in 0..n_creatures {
        fs::write(creatures_dir.join(format!("creature-{n:03}.json")), &json).unwrap();
    }
    write_synthetic_bin(&data_dir, num_inputs, num_outputs, total_bytes);

    let mut group = c.benchmark_group("large_creature_cpu_vs_gpu");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.throughput(Throughput::Bytes(total_bytes as u64));

    // Before: CPU+PGO path.
    group.bench_function(BenchmarkId::new("cpu", n_creatures), |b| {
        b.iter(|| {
            let result = score_from_creature_dir(
                &creatures_dir,
                &data_dir,
                GpuBackendLabel::CpuFallback,
                CostKind::default(),
            )
            .expect("cpu large-creature score");
            black_box(result);
        });
    });

    // After: GPU `forward_mse_scratch` path.
    let backend = resolve_backend(GpuMode::Auto).unwrap_or(GpuBackendLabel::CpuFallback);
    if backend == GpuBackendLabel::CpuFallback {
        eprintln!("large_creature_cpu_vs_gpu: no GPU adapter — skipping GPU row");
        group.finish();
        return;
    }
    let ctx = match select_adapter() {
        Ok(Some(c)) => Arc::new(c),
        _ => {
            eprintln!("large_creature_cpu_vs_gpu: select_adapter returned no context");
            group.finish();
            return;
        }
    };
    group.bench_function(BenchmarkId::new("gpu", n_creatures), |b| {
        let ctx = ctx.clone();
        b.iter(|| {
            let result = score_from_creature_dir_gpu(
                &creatures_dir,
                &data_dir,
                backend,
                ctx.clone(),
                2,
                CostKind::default(),
            )
            .expect("gpu large-creature score");
            black_box(result);
        });
    });
    group.finish();
}

/// Issue #467: shallow-creature CPU vs GPU comparison. The `Enceladus` island
/// shape (2461 inputs → 16 hidden → 1 output, ≈ 12 180 point-wise synapses)
/// exceeds the 256-neuron private cap (inputs count) so it routes to the GPU
/// `forward_mse_scratch` kernel, exactly like the deep production creature, but
/// does far less per-record work. This group A/Bs `score_from_creature_dir`
/// (CPU, `--gpu off`) against `score_from_creature_dir_gpu` (`--gpu on`) on a
/// synthetic stand-in built by [`rust_scorer::shallow_fixture`], so the negative
/// result is reproducible without the private `Enceladus` creature.
///
/// Sized via `BENCH_SHALLOW_HIDDEN` (default 16), `BENCH_SHALLOW_SYNAPSES`
/// (default 12180), `BENCH_SHALLOW_CREATURES` (default 50 — a realistic island
/// population; set 63 to match the #317 sweep) and `BENCH_SHALLOW_BYTES`
/// (default 32 MiB). When no GPU adapter is present the GPU row is skipped so
/// CPU-only CI runners still pass.
fn bench_shallow_gpu_vs_cpu(c: &mut Criterion) {
    let hidden = env_usize("BENCH_SHALLOW_HIDDEN", 16).max(1);
    let synapses = env_usize("BENCH_SHALLOW_SYNAPSES", 12_180);
    let n_creatures = env_usize("BENCH_SHALLOW_CREATURES", 50).max(1);
    let total_bytes = env_usize("BENCH_SHALLOW_BYTES", 32 * 1024 * 1024);
    // The Enceladus record shape: 2461 inputs / 1 output.
    let num_inputs = env_usize("BENCH_SHALLOW_INPUTS", 2461).max(1);
    let num_outputs = 1;

    let tmp = TempDir::new().expect("tempdir");
    let creatures_dir = tmp.path().join("creatures");
    fs::create_dir_all(&creatures_dir).unwrap();
    let data_dir = tmp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();

    let json = rust_scorer::shallow_fixture::shallow_creature_json(
        num_inputs,
        num_outputs,
        hidden,
        synapses,
    );
    for n in 0..n_creatures {
        fs::write(creatures_dir.join(format!("creature-{n:03}.json")), &json).unwrap();
    }
    write_synthetic_bin(&data_dir, num_inputs, num_outputs, total_bytes);

    let mut group = c.benchmark_group("shallow_gpu_vs_cpu");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.throughput(Throughput::Bytes(total_bytes as u64));

    // CPU path (`--gpu off`).
    group.bench_function(BenchmarkId::new("cpu", n_creatures), |b| {
        b.iter(|| {
            let result = score_from_creature_dir(
                &creatures_dir,
                &data_dir,
                GpuBackendLabel::CpuFallback,
                CostKind::default(),
            )
            .expect("cpu shallow-creature score");
            black_box(result);
        });
    });

    // GPU path (`--gpu on`, `forward_mse_scratch`).
    let backend = resolve_backend(GpuMode::Auto).unwrap_or(GpuBackendLabel::CpuFallback);
    if backend == GpuBackendLabel::CpuFallback {
        eprintln!("shallow_gpu_vs_cpu: no GPU adapter — skipping GPU row");
        group.finish();
        return;
    }
    let ctx = match select_adapter() {
        Ok(Some(c)) => Arc::new(c),
        _ => {
            eprintln!("shallow_gpu_vs_cpu: select_adapter returned no context");
            group.finish();
            return;
        }
    };
    group.bench_function(BenchmarkId::new("gpu", n_creatures), |b| {
        let ctx = ctx.clone();
        b.iter(|| {
            let result = score_from_creature_dir_gpu(
                &creatures_dir,
                &data_dir,
                backend,
                ctx.clone(),
                2,
                CostKind::default(),
            )
            .expect("gpu shallow-creature score");
            black_box(result);
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// Issue #296 — production-scale fixture (evolved production creature).
//
// Every candidate optimisation (#297–#299) is gated against the **production**
// creature, not the synthetic 8→8→2 fixture above. The evolved production
// network (≈ 1666 neurons across ≈ 34 squash types, ≈ 21 510 synapses, 2461
// inputs / 1 output) profiles very differently from pure TANH.
//
// This public repo ships no production creature and fetches nothing at bench
// time (Issue #448). Supply a **local** copy via `BENCH_PROD_CREATURE`; when it
// is unset the production benches skip cleanly, exactly like the GPU benches
// skip without an adapter.
//
// Fail-loud: once a local creature is supplied, if it cannot be read /
// deserialized, or its topology is not production-sized, the fixture panics — it
// never falls back to the synthetic creature, which would corrupt every
// downstream A/B comparison.
// ---------------------------------------------------------------------------

struct ProdFixture {
    _tmp: TempDir,
    creature_json: String,
    creature: neat_core::creature::CreatureExport,
    creatures_root: PathBuf,
    data_dir: PathBuf,
    num_inputs: usize,
    num_outputs: usize,
    total_bytes: usize,
}

/// Lazily build the production fixture once per process.
///
/// Returns `None` when no local production creature is supplied via
/// `BENCH_PROD_CREATURE` (the public repo ships none and fetches nothing —
/// Issue #448), so the production benches skip cleanly. When a local creature
/// **is** supplied, this fails loud on any problem so a broken fixture can never
/// masquerade as a valid measurement.
fn prod_fixture() -> Option<&'static ProdFixture> {
    static FIX: OnceLock<Option<ProdFixture>> = OnceLock::new();
    FIX.get_or_init(|| {
        let Some(creature_path) = production_creature_path_from_env() else {
            eprintln!(
                "prod_fixture: {} is unset — skipping production benches. \
                 Set it to a local network.json to run them (nothing is fetched).",
                rust_scorer::prod_fixture::PROD_CREATURE_ENV,
            );
            return None;
        };

        if !creature_path.exists() {
            eprintln!(
                "prod_fixture: {}={} does not exist — skipping production benches.",
                rust_scorer::prod_fixture::PROD_CREATURE_ENV,
                creature_path.display(),
            );
            return None;
        }

        let creature_json = fs::read_to_string(&creature_path).unwrap_or_else(|e| {
            panic!(
                "failed to read production creature at {}: {e}",
                creature_path.display()
            )
        });

        // Parse + topology-check — panics (never falls back) on any failure.
        let creature = load_production_creature(&creature_json).unwrap_or_else(|e| {
            panic!(
                "production creature at {} failed the fail-loud load: {e}",
                creature_path.display()
            )
        });

        let num_inputs = creature.input;
        let num_outputs = creature.output;
        let total_bytes = env_usize(
            "BENCH_PROD_BYTES",
            rust_scorer::prod_fixture::DEFAULT_BENCH_CORPUS_BYTES,
        );

        let tmp = TempDir::new().expect("tempdir");
        let data_dir = tmp.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();
        write_synthetic_bin(&data_dir, num_inputs, num_outputs, total_bytes);

        // Sanity-assert the corpus row count before any timing starts, so a
        // regression that silently shrinks the corpus fails here rather than
        // producing plausible-but-meaningless numbers.
        let expected_rows = corpus_record_count(total_bytes, num_inputs, num_outputs);
        let bin_files = find_bin_files(&data_dir).expect("find prod bin files");
        let actual_bytes: u64 = bin_files
            .iter()
            .map(|p| fs::metadata(p).expect("bin metadata").len())
            .sum();
        let record_bytes = (num_inputs + num_outputs) * std::mem::size_of::<f32>();
        let actual_rows = (actual_bytes as usize) / record_bytes;
        assert_eq!(
            actual_rows, expected_rows,
            "production corpus row count {actual_rows} != expected {expected_rows}"
        );

        // Multi-creature root seeded with copies of the production creature.
        let creatures_root = tmp.path().join("creatures-root");
        fs::create_dir_all(&creatures_root).unwrap();
        let pool_size = env_usize("BENCH_PROD_CREATURES", 4).max(1);
        for n in 0..pool_size {
            fs::write(
                creatures_root.join(format!("creature-{n:03}.json")),
                &creature_json,
            )
            .unwrap();
        }

        eprintln!(
            "prod_fixture: {} neurons, {} synapses, {num_inputs} inputs, {num_outputs} outputs; \
             corpus {actual_bytes} bytes / {actual_rows} records; {pool_size} creature copies",
            creature.neurons.len(),
            creature.synapses.len(),
        );

        Some(ProdFixture {
            _tmp: tmp,
            creature_json,
            creature,
            creatures_root,
            data_dir,
            num_inputs,
            num_outputs,
            total_bytes,
        })
    })
    .as_ref()
}

/// Single-creature forward-only fused path against the production creature.
fn bench_production_single(c: &mut Criterion) {
    let Some(fix) = prod_fixture() else {
        return;
    };
    let creature = &fix.creature;
    let bin_files = find_bin_files(&fix.data_dir).expect("find bin files");
    let config = TrainingDataConfig {
        num_inputs: fix.num_inputs,
        num_outputs: fix.num_outputs,
    };

    let mut group = c.benchmark_group("production_single_creature");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));
    group.throughput(Throughput::Bytes(fix.total_bytes as u64));
    group.bench_function("forward_only", |b| {
        b.iter_batched(
            || compile_creature(creature).expect("compile"),
            |mut net| {
                let r = accumulate_cost_sum_forward_only_fused(
                    CostKind::Mse,
                    &bin_files,
                    &config,
                    &mut net,
                )
                .expect("fused MSE accumulate");
                black_box(r);
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

/// Directory mode against copies of the production creature. Population sizes
/// are kept small (the production creature is ≈ 1666 neurons) and overridable
/// via `BENCH_PROD_CREATURES`.
fn bench_production_multi(c: &mut Criterion) {
    let Some(fix) = prod_fixture() else {
        return;
    };
    let mut group = c.benchmark_group("production_multi_creature");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.throughput(Throughput::Bytes(fix.total_bytes as u64));

    let pool_size = env_usize("BENCH_PROD_CREATURES", 4).max(1);
    for &n in &[1_usize, pool_size] {
        if n > pool_size {
            continue;
        }
        let sub_dir = fix
            .creatures_root
            .parent()
            .unwrap()
            .join(format!("prod-dir-{n}"));
        if !sub_dir.exists() {
            fs::create_dir_all(&sub_dir).unwrap();
            for i in 0..n {
                fs::write(
                    sub_dir.join(format!("creature-{i:03}.json")),
                    &fix.creature_json,
                )
                .unwrap();
            }
        }

        group.bench_with_input(BenchmarkId::new("creatures", n), &sub_dir, |b, dir| {
            b.iter(|| {
                let result = score_from_creature_dir(
                    dir,
                    &fix.data_dir,
                    GpuBackendLabel::CpuFallback,
                    CostKind::default(),
                )
                .expect("multi-creature score");
                black_box(result);
            });
        });
    }
    group.finish();
}

/// Issue #312: production-scale creature CPU↔GPU A/B. The real creature
/// carries the aggregate squashes (MINIMUM/MAXIMUM/IF) and constant neurons
/// that #312 taught the kernels to host, so — unlike `bench_production_multi`,
/// which stays CPU-only — this directly compares directory-mode GPU against the
/// CPU pipeline on the actual production blocker. Skips the GPU row cleanly when
/// no adapter is present. The `gpu`/`cpu` rows drive the GPU-default decision
/// (parent NEAT-AI#3256): recommend the default flip only if the GPU row is
/// demonstrably faster with non-overlapping CIs.
fn bench_production_gpu_vs_cpu(c: &mut Criterion) {
    let Some(fix) = prod_fixture() else {
        return;
    };
    let pool_size = env_usize("BENCH_PROD_CREATURES", 4).max(1);

    let mut group = c.benchmark_group("production_gpu_vs_cpu");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.throughput(Throughput::Bytes(fix.total_bytes as u64));

    // CPU baseline over the production creature pool.
    group.bench_function(BenchmarkId::new("cpu", pool_size), |b| {
        b.iter(|| {
            let result = score_from_creature_dir(
                &fix.creatures_root,
                &fix.data_dir,
                GpuBackendLabel::CpuFallback,
                CostKind::default(),
            )
            .expect("cpu prod score");
            black_box(result);
        });
    });

    let backend = resolve_backend(GpuMode::Auto).unwrap_or(GpuBackendLabel::CpuFallback);
    if backend == GpuBackendLabel::CpuFallback {
        eprintln!("production_gpu_vs_cpu: no GPU adapter — skipping GPU row");
        group.finish();
        return;
    }
    let ctx = match select_adapter() {
        Ok(Some(c)) => Arc::new(c),
        _ => {
            eprintln!("production_gpu_vs_cpu: select_adapter returned no context");
            group.finish();
            return;
        }
    };
    group.bench_function(BenchmarkId::new("gpu", pool_size), |b| {
        let ctx = ctx.clone();
        b.iter(|| {
            let result = score_from_creature_dir_gpu(
                &fix.creatures_root,
                &fix.data_dir,
                backend,
                ctx.clone(),
                2,
                CostKind::default(),
            )
            .expect("gpu prod score");
            black_box(result);
        });
    });
    group.finish();
}

/// Issue #308: directory-mode early-exit A/B. For each population `N` this
/// benches the full-score baseline (`score_from_creature_dir`) against the
/// early-exit path (`score_from_creature_dir_with_early_exit`) with a callback
/// that aborts ~50 % of the population after the first scored chunk — the
/// representative "early-exit fires on ≥30 % of the population" scenario from
/// the issue. Criterion's report lets you read the wall-clock delta between the
/// `full` and `abort50` rows; the merge gate is ≥5 % on `abort50`.
fn bench_early_exit_directory(c: &mut Criterion) {
    let fix = fixture();
    let mut group = c.benchmark_group("early_exit_directory");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));
    group.throughput(Throughput::Bytes(fix.total_bytes as u64));

    for &n in &[50_usize, 200_usize] {
        let sub_dir = fix
            .creatures_root
            .parent()
            .unwrap()
            .join(format!("ee-dir-{n}"));
        if !sub_dir.exists() {
            fs::create_dir_all(&sub_dir).unwrap();
            for i in 0..n {
                let src = fix.creatures_root.join(format!("creature-{i:03}.json"));
                let dst = sub_dir.join(format!("creature-{i:03}.json"));
                fs::copy(&src, &dst).expect("copy creature");
            }
        }

        // Full-score baseline.
        group.bench_with_input(BenchmarkId::new("full", n), &sub_dir, |b, dir| {
            b.iter(|| {
                let result = score_from_creature_dir(
                    dir,
                    &fix.data_dir,
                    GpuBackendLabel::CpuFallback,
                    CostKind::default(),
                )
                .expect("full-score");
                black_box(result);
            });
        });

        // Early-exit: abort ~50 % of the population (even indices) after the
        // first callback fires, then let the rest run to completion.
        group.bench_with_input(BenchmarkId::new("abort50", n), &sub_dir, |b, dir| {
            b.iter(|| {
                let mut aborted = false;
                let result = score_from_creature_dir_with_early_exit(
                    dir,
                    &fix.data_dir,
                    GpuBackendLabel::CpuFallback,
                    CostKind::default(),
                    |partials| {
                        if !aborted {
                            aborted = true;
                            let losers: Vec<usize> = partials
                                .iter()
                                .map(|p| p.creature_index)
                                .filter(|i| i % 2 == 0)
                                .collect();
                            return EarlyExit::AbortCreatures(losers);
                        }
                        EarlyExit::Continue
                    },
                )
                .expect("early-exit score");
                black_box(result);
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_score_from_json_fused,
    bench_score_from_creature_dir,
    bench_early_exit_directory,
    bench_unpack_and_mse_inner,
    bench_gpu_score_from_creature_dir,
    bench_gpu_pipelining_toggle,
    bench_large_creature_cpu_vs_gpu,
    bench_shallow_gpu_vs_cpu,
    bench_production_single,
    bench_production_multi,
    bench_production_gpu_vs_cpu,
);
criterion_main!(benches);
