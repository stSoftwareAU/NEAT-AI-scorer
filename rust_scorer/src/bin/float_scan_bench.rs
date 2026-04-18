//! Throwaway micro-benchmark: scan `.bin` training files and sum all `f32` values
//! (trivial work). Compares `read_copy` vs `mmap` vs `double_buf` I/O patterns.
//!
//! Build: `cargo build --release -p rust_scorer --bin float_scan_bench`

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use clap::{Parser, ValueEnum};
use memmap2::MmapOptions;
use neat_core::training_data::find_bin_files;

const TARGET_READ_BYTES: usize = 2 * 1024 * 1024;
const PENDING_COMPACT_HEAD_BYTES: usize = 512 * 1024;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Mode {
    ReadCopy,
    Mmap,
    DoubleBuf,
}

#[derive(Parser, Debug)]
#[command(name = "float_scan_bench")]
struct Cli {
    /// Directory containing `.bin` files (numeric sort, same as training pipeline).
    data_dir: PathBuf,

    /// Total `f32` values per record (inputs + outputs).
    #[arg(long)]
    floats_per_record: usize,

    #[arg(long, value_enum, default_value_t = Mode::ReadCopy)]
    mode: Mode,

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

fn scan_read_copy(bin_files: &[PathBuf], record_bytes: usize) -> f64 {
    let read_buf_len = (TARGET_READ_BYTES / record_bytes * record_bytes).max(record_bytes);
    let mut sum = 0.0_f64;
    let mut pending: Vec<u8> = Vec::new();
    let mut head: usize = 0;
    let mut read_buf = vec![0u8; read_buf_len];

    for path in bin_files {
        let mut file = File::open(path).expect("open");
        loop {
            compact_pending(&mut pending, &mut head);

            let n = file.read(&mut read_buf).expect("read");
            if n > 0 {
                pending.extend_from_slice(&read_buf[..n]);
            }
            drain_complete_records(&pending, &mut head, record_bytes, &mut sum);

            if n == 0 {
                assert_eq!(head, pending.len(), "trailing bytes in {}", path.display());
                pending.clear();
                head = 0;
                break;
            }
        }
    }
    sum
}

fn scan_mmap(bin_files: &[PathBuf], record_bytes: usize) -> f64 {
    let mut sum = 0.0_f64;
    for path in bin_files {
        let file = File::open(path).expect("open");
        let mmap = unsafe { MmapOptions::new().map(&file).expect("mmap") };
        let len = mmap.len();
        assert_eq!(
            len % record_bytes,
            0,
            "file size not multiple of record: {}",
            path.display()
        );
        sum += sum_f32_le_bytes(&mmap[..]);
    }
    sum
}

enum Chunk {
    /// Bytes read from current file (`n` may be 0 at EOF for that file).
    Part(Vec<u8>, usize),
    /// No more files; `pending` must be fully drained.
    AllFilesDone,
}

fn scan_double_buf(bin_files: &[PathBuf], record_bytes: usize) -> f64 {
    let read_buf_len = (TARGET_READ_BYTES / record_bytes * record_bytes).max(record_bytes);
    let files: Vec<PathBuf> = bin_files.to_vec();

    // `empty` must accept two returned buffers while the reader may still hold one in flight.
    let (fill_tx, fill_rx) = mpsc::sync_channel::<Chunk>(1);
    let (empty_tx, empty_rx) = mpsc::sync_channel::<Vec<u8>>(2);

    empty_tx.send(vec![0u8; read_buf_len]).expect("seed");
    empty_tx.send(vec![0u8; read_buf_len]).expect("seed");

    let handle = thread::spawn(move || {
        for path in &files {
            let mut file = File::open(path).expect("open");
            loop {
                let mut buf = match empty_rx.recv() {
                    Ok(b) => b,
                    Err(_) => return,
                };
                let n = match file.read(&mut buf) {
                    Ok(n) => n,
                    Err(_) => {
                        let _ = fill_tx.send(Chunk::Part(buf, 0));
                        return;
                    }
                };
                if fill_tx.send(Chunk::Part(buf, n)).is_err() {
                    return;
                }
                if n == 0 {
                    break;
                }
            }
        }
        let _ = fill_tx.send(Chunk::AllFilesDone);
    });

    let mut sum = 0.0_f64;
    let mut pending: Vec<u8> = Vec::new();
    let mut head: usize = 0;

    loop {
        match fill_rx.recv().expect("chunk") {
            Chunk::Part(buf, n) => {
                if n > 0 {
                    pending.extend_from_slice(&buf[..n]);
                }
                if empty_tx.send(buf).is_err() {
                    break;
                }
                compact_pending(&mut pending, &mut head);
                drain_complete_records(&pending, &mut head, record_bytes, &mut sum);
            }
            Chunk::AllFilesDone => {
                assert_eq!(head, pending.len(), "trailing bytes after AllFilesDone");
                break;
            }
        }
    }

    drop(empty_tx);
    let _ = handle.join();
    sum
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

    let scan = || match cli.mode {
        Mode::ReadCopy => scan_read_copy(&bin_files, record_bytes),
        Mode::Mmap => scan_mmap(&bin_files, record_bytes),
        Mode::DoubleBuf => scan_double_buf(&bin_files, record_bytes),
    };

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
            "mode": format!("{:?}", cli.mode),
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
