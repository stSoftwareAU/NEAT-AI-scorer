//! `NEAT_SCORER_READ_BYTES` tuning for chunked `.bin` reads (aligned to whole records).
//!
//! `neat_core::training_bin_stream` exposes a single [`for_each_read_chunk`] path; buffer sizing
//! stays configurable here so callers can widen reads for parallel activation batches.
//!
//! [`for_each_read_chunk`]: neat_core::training_bin_stream::for_each_read_chunk

const DEFAULT_READ_BYTES: usize = 2 * 1024 * 1024;

/// Upper bound for read buffer size (matches previous `neat_core` tuner cap).
pub const MAX_READ_BYTES: usize = 64 * 1024 * 1024;

/// Target bytes per `read` (rounded down to a multiple of `record_bytes`).
pub fn training_read_target_bytes_from_env(record_bytes: usize) -> usize {
    let rb = record_bytes.max(1);
    let raw = std::env::var("NEAT_SCORER_READ_BYTES")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_READ_BYTES)
        .clamp(rb, MAX_READ_BYTES);
    (raw / rb) * rb
}

/// Stable label for the active training-read implementation (for JSON diagnostics).
pub fn training_read_backend_label() -> &'static str {
    #[cfg(target_arch = "wasm32")]
    {
        "wasm_chunked"
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        "native_pipelined"
    }
}
