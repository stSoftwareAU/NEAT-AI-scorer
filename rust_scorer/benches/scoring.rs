//! Criterion benchmarks for the `rust_scorer` hot paths — Issue #36.
//!
//! Three groups, all built on a shared lazily-created tempdir fixture so the
//! synthetic creature(s) and `.bin` corpus are paid for **once per process**:
//!
//! * `score_from_json_fused` — forward-only fused path, exercised through
//!   [`rust_scorer::stream_score::accumulate_mse_sum_forward_only_fused`] (the
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

use rust_scorer::multi_score::score_from_creature_dir;
use rust_scorer::stream_score::accumulate_mse_sum_forward_only_fused;

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

/// Build a forward-only synthetic creature JSON wired as a small dense MLP.
fn synthetic_creature_json(num_inputs: usize, num_outputs: usize, hidden: usize) -> String {
    let mut neurons: Vec<String> = Vec::with_capacity(hidden + num_outputs);
    for h in 0..hidden {
        neurons.push(format!(
            r#"{{"type":"hidden","uuid":"hidden-{h}","bias":0.05,"squash":"TANH"}}"#
        ));
    }
    for o in 0..num_outputs {
        neurons.push(format!(
            r#"{{"type":"output","uuid":"output-{o}","bias":0.0,"squash":"IDENTITY"}}"#
        ));
    }

    let mut synapses: Vec<String> = Vec::with_capacity(num_inputs * hidden + hidden * num_outputs);
    // Input -> hidden
    for i in 0..num_inputs {
        for h in 0..hidden {
            // Vary weight slightly so activations are non-degenerate.
            let w = 0.05 + 0.001 * ((i * hidden + h) as f64);
            synapses.push(format!(
                r#"{{"fromUUID":"input-{i}","toUUID":"hidden-{h}","weight":{w}}}"#
            ));
        }
    }
    // Hidden -> output
    for h in 0..hidden {
        for o in 0..num_outputs {
            let w = 0.1 + 0.001 * ((h * num_outputs + o) as f64);
            synapses.push(format!(
                r#"{{"fromUUID":"hidden-{h}","toUUID":"output-{o}","weight":{w}}}"#
            ));
        }
    }

    format!(
        r#"{{"input":{num_inputs},"output":{num_outputs},"forwardOnly":true,"semanticVersion":"4.0.0","neurons":[{}],"synapses":[{}]}}"#,
        neurons.join(","),
        synapses.join(","),
    )
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
                let r =
                    accumulate_mse_sum_forward_only_fused(&bin_files, &config, &creature, &mut net)
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
                let result =
                    score_from_creature_dir(dir, &fix.data_dir).expect("multi-creature score");
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

criterion_group!(
    benches,
    bench_score_from_json_fused,
    bench_score_from_creature_dir,
    bench_unpack_and_mse_inner,
);
criterion_main!(benches);
