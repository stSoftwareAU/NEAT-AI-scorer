//! Micro-benchmark: scan `.bin` training files and sum all `f32` values (trivial work).
//! Uses `training_bin_stream::for_each_read_chunk` with the same **`NEAT_SCORER_READ_BYTES`**
//! env tuning as `rust_scorer` / `stream_score`.
//!
//! Build: `cargo build --release -p rust_scorer --bin float_scan_bench`

#[path = "../env_tuning.rs"]
mod env_tuning;
#[path = "../read_tuning.rs"]
mod read_tuning;

use std::path::{Path, PathBuf};

use clap::Parser;
use neat_core::training_bin_stream::for_each_read_chunk;
use neat_core::training_data::find_bin_files;

use read_tuning::{training_read_backend_label, training_read_target_bytes_from_env};

const PENDING_COMPACT_HEAD_BYTES: usize = 512 * 1024;

#[derive(Parser, Debug)]
#[command(name = "float_scan_bench")]
struct Cli {
    /// Directory containing `.bin` files (numeric sort, same as training pipeline).
    data_dir: PathBuf,

    /// Total `f32` values per record (inputs + outputs).
    #[arg(long)]
    floats_per_record: usize,

    /// Number of timed iterations (median reported).
    #[arg(long, default_value_t = 7)]
    runs: usize,
}

#[inline]
fn sum_f32_le_bytes(data: &[u8]) -> f64 {
    debug_assert_eq!(data.len() % 4, 0);
    let n = data.len() / 4;
    let mut acc = 0.0_f64;
    #[cfg(target_endian = "little")]
    {
        let p = data.as_ptr();
        for i in 0..n {
            // SAFETY: `data.len()` is a multiple of 4 (asserted above) and
            // `i < n == data.len() / 4`, so `p.add(i * 4)` plus a 4-byte
            // unaligned read stays within `data`.
            let bits = unsafe { p.add(i * 4).cast::<u32>().read_unaligned() };
            acc += f32::from_bits(bits) as f64;
        }
    }
    #[cfg(not(target_endian = "little"))]
    {
        for q in data.chunks_exact(4) {
            acc += f32::from_le_bytes([q[0], q[1], q[2], q[3]]) as f64;
        }
    }
    acc
}

#[inline]
fn compact_pending(pending: &mut Vec<u8>, head: &mut usize) {
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

fn drain_complete_records(pending: &[u8], head: &mut usize, record_bytes: usize, total: &mut f64) {
    loop {
        let avail = pending.len() - *head;
        let complete_len = (avail / record_bytes) * record_bytes;
        if complete_len == 0 {
            break;
        }
        *total += sum_f32_le_bytes(&pending[*head..*head + complete_len]);
        *head += complete_len;
    }
}

fn scan_bin_files(bin_files: &[PathBuf], record_bytes: usize) -> Result<f64, String> {
    let target = training_read_target_bytes_from_env(record_bytes);
    let read_buf_len = (target / record_bytes * record_bytes).max(record_bytes);
    let mut sum = 0.0_f64;
    let mut pending: Vec<u8> = Vec::new();
    let mut head: usize = 0;

    for_each_read_chunk(bin_files, read_buf_len, |chunk| {
        if !chunk.is_empty() {
            pending.extend_from_slice(chunk);
        }
        compact_pending(&mut pending, &mut head);
        drain_complete_records(&pending, &mut head, record_bytes, &mut sum);
        Ok(())
    })?;

    if head != pending.len() {
        return Err(format!(
            "trailing {} bytes (incomplete record)",
            pending.len() - head
        ));
    }
    Ok(sum)
}

fn median_ms(times: &mut [f64]) -> f64 {
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mid = times.len() / 2;
    if times.len() % 2 == 1 {
        times[mid]
    } else {
        (times[mid - 1] + times[mid]) / 2.0
    }
}

fn main() {
    let cli = Cli::parse();
    if cli.floats_per_record == 0 {
        eprintln!("--floats-per-record must be > 0");
        std::process::exit(1);
    }
    let record_bytes = cli.floats_per_record * std::mem::size_of::<f32>();

    let data_dir = Path::new(&cli.data_dir);
    let bin_files = find_bin_files(data_dir).expect("list bin files");
    if bin_files.is_empty() {
        eprintln!("No .bin files in {}", cli.data_dir.display());
        std::process::exit(1);
    }

    let total_bytes: u64 = bin_files
        .iter()
        .map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .sum();

    let bench_target = training_read_target_bytes_from_env(record_bytes);
    let bench_read_buf_len = (bench_target / record_bytes * record_bytes).max(record_bytes);

    let scan = || scan_bin_files(&bin_files, record_bytes).expect("scan");

    let _ = scan();

    let mut times = Vec::with_capacity(cli.runs);
    let mut checksum = 0.0_f64;

    for _ in 0..cli.runs {
        let t0 = std::time::Instant::now();
        checksum = scan();
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
    }

    let med = median_ms(&mut times);
    let gb = total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let gbps = if med > 0.0 { gb / (med / 1000.0) } else { 0.0 };

    println!(
        "{}",
        serde_json::json!({
            "trainingReadBackend": training_read_backend_label(),
            "readBufLen": bench_read_buf_len,
            "dataDir": cli.data_dir,
            "floatsPerRecord": cli.floats_per_record,
            "recordBytes": record_bytes,
            "fileCount": bin_files.len(),
            "totalBytes": total_bytes,
            "runs": cli.runs,
            "timesMs": times.iter().map(|t| (t * 100.0).round() / 100.0).collect::<Vec<_>>(),
            "medianMs": (med * 100.0).round() / 100.0,
            "approxThroughputGiBs": (gbps * 100.0).round() / 100.0,
            "checksum": checksum,
        })
    );
}

#[cfg(test)]
mod tests {
    use super::sum_f32_le_bytes;

    /// Encode `f32`s as little-endian bytes the way the bench files store them.
    fn le_bytes(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn sums_empty_slice_to_zero() {
        assert_eq!(sum_f32_le_bytes(&[]), 0.0);
    }

    #[test]
    fn sums_single_value() {
        assert_eq!(sum_f32_le_bytes(&le_bytes(&[1.5])), 1.5);
    }

    #[test]
    fn sums_multiple_values_exercising_pointer_arithmetic() {
        // Several elements force the `unsafe { p.add(i * 4) ... }` loop to
        // advance the pointer across every in-bounds offset.
        let values = [1.0_f32, 2.5, -3.25, 4.75, 0.0, 100.0];
        let expected: f64 = values.iter().map(|v| f64::from(*v)).sum();
        assert_eq!(sum_f32_le_bytes(&le_bytes(&values)), expected);
    }
}
