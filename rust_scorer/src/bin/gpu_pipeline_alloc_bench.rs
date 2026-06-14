//! Allocator-pressure benchmark for the pipelined GPU directory path (Issue #202).
//!
//! Builds a small synthetic creatures directory plus a multi-chunk `.bin` file,
//! then scores it through [`rust_scorer::multi_score::score_from_creature_dir_gpu`]
//! with `inflight_chunks == 2` (the pipelined worker-thread path). A counting
//! global allocator records how many heap allocations the run performs.
//!
//! The pipelined path used to clone every chunk via `floats.to_vec()` on the hot
//! I/O thread — one allocation + memcpy per chunk. Issue #202 swaps a recycled
//! buffer into the unpack slot instead, so the per-chunk allocation disappears.
//! Run this bench before and after the change to see the allocation count drop by
//! roughly the number of streamed chunks.
//!
//! Build: `cargo build --release -p rust_scorer --bin gpu_pipeline_alloc_bench`
//! Run:   `./target/release/gpu_pipeline_alloc_bench`
//!
//! Skips cleanly (exit 0, prints a notice) when no GPU adapter is available, so
//! it is safe to invoke on CPU-only hosts.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use rust_scorer::cost::CostKind;
use rust_scorer::gpu::{GpuBackendLabel, select_adapter};
use rust_scorer::multi_score::score_from_creature_dir_gpu;

/// Global allocator that tallies allocation count and bytes while `ACTIVE` is set.
struct CountingAllocator;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);
static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards to the System allocator unchanged; the only
// added behaviour is two relaxed atomic counters, which cannot affect memory
// safety of the returned pointers.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const NUM_INPUTS: usize = 8;
const NUM_OUTPUTS: usize = 2;
const NUM_CREATURES: usize = 8;
const NUM_RECORDS: usize = 100_000;
const HIDDEN: usize = 8;
// Small read buffer → many streamed chunks → many per-chunk submissions.
const READ_BYTES: usize = (NUM_INPUTS + NUM_OUTPUTS) * 4 * 64;

fn synthetic_creature_json() -> String {
    let mut neurons: Vec<String> = Vec::new();
    for h in 0..HIDDEN {
        neurons.push(format!(
            r#"{{"type":"hidden","uuid":"hidden-{h}","bias":0.05,"squash":"TANH"}}"#
        ));
    }
    for o in 0..NUM_OUTPUTS {
        neurons.push(format!(
            r#"{{"type":"output","uuid":"output-{o}","bias":0.0,"squash":"IDENTITY"}}"#
        ));
    }
    let mut synapses: Vec<String> = Vec::new();
    for i in 0..NUM_INPUTS {
        for h in 0..HIDDEN {
            let w = 0.05 + 0.001 * ((i * HIDDEN + h) as f64);
            synapses.push(format!(
                r#"{{"fromUUID":"input-{i}","toUUID":"hidden-{h}","weight":{w}}}"#
            ));
        }
    }
    for h in 0..HIDDEN {
        for o in 0..NUM_OUTPUTS {
            let w = 0.1 + 0.001 * ((h * NUM_OUTPUTS + o) as f64);
            synapses.push(format!(
                r#"{{"fromUUID":"hidden-{h}","toUUID":"output-{o}","weight":{w}}}"#
            ));
        }
    }
    format!(
        r#"{{"input":{NUM_INPUTS},"output":{NUM_OUTPUTS},"forwardOnly":true,"semanticVersion":"4.0.0","neurons":[{}],"synapses":[{}]}}"#,
        neurons.join(","),
        synapses.join(","),
    )
}

fn build_fixture(root: &Path) -> std::io::Result<(std::path::PathBuf, std::path::PathBuf)> {
    let creatures_dir = root.join("creatures");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&creatures_dir)?;
    std::fs::create_dir_all(&data_dir)?;

    let json = synthetic_creature_json();
    for c in 0..NUM_CREATURES {
        std::fs::write(creatures_dir.join(format!("creature-{c}.json")), &json)?;
    }

    let mut bytes = Vec::with_capacity(NUM_RECORDS * (NUM_INPUTS + NUM_OUTPUTS) * 4);
    for i in 0..NUM_RECORDS {
        for k in 0..(NUM_INPUTS + NUM_OUTPUTS) {
            let v = ((i.wrapping_mul(31) + k) as f32 * 1.0e-3).sin();
            bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    let mut f = std::fs::File::create(data_dir.join("0.bin"))?;
    f.write_all(&bytes)?;
    f.flush()?;

    Ok((creatures_dir, data_dir))
}

fn main() {
    // Probe for a GPU first; skip cleanly on CPU-only hosts.
    let ctx = match select_adapter() {
        Ok(Some(c)) if c.backend != GpuBackendLabel::CpuFallback => Arc::new(c),
        _ => {
            println!("gpu_pipeline_alloc_bench: no compatible GPU adapter — skipping");
            return;
        }
    };
    let backend = ctx.backend;

    let root = std::env::temp_dir().join("gpu_pipeline_alloc_bench");
    let _ = std::fs::remove_dir_all(&root);
    let (creatures_dir, data_dir) = build_fixture(&root).expect("build fixture");

    // Force many streamed chunks so the per-chunk allocation dominates the delta.
    // SAFETY: single-threaded setup before any worker threads are spawned.
    unsafe {
        std::env::set_var("NEAT_SCORER_READ_BYTES", READ_BYTES.to_string());
    }

    // Start counting only around the scored run, excluding fixture + adapter setup.
    ALLOC_COUNT.store(0, Ordering::Relaxed);
    ALLOC_BYTES.store(0, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
    let started = Instant::now();
    let results = score_from_creature_dir_gpu(
        &creatures_dir,
        &data_dir,
        backend,
        ctx,
        2, // pipelined path
        CostKind::Mse,
    )
    .expect("score directory on GPU");
    let elapsed = started.elapsed().as_secs_f64();
    ACTIVE.store(false, Ordering::Relaxed);

    let allocs = ALLOC_COUNT.load(Ordering::Relaxed);
    let bytes = ALLOC_BYTES.load(Ordering::Relaxed);
    let dispatches = results
        .values()
        .next()
        .and_then(|r| r.gpu_dispatch_count)
        .unwrap_or(0);

    let _ = std::fs::remove_dir_all(&root);

    println!("gpu_pipeline_alloc_bench ({} backend)", backend.as_str());
    println!("  creatures           : {NUM_CREATURES}");
    println!("  records             : {NUM_RECORDS}");
    println!("  read_bytes          : {READ_BYTES}");
    println!("  gpu_dispatch_count  : {dispatches}");
    println!("  allocations (scored): {allocs}");
    println!("  alloc_bytes (scored): {bytes}");
    println!("  elapsed_secs        : {elapsed:.4}");
}
