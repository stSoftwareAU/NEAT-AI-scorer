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
///
/// # Examples
///
/// ```
/// use rust_scorer::read_tuning::training_read_target_bytes_from_env;
///
/// // Whatever `NEAT_SCORER_READ_BYTES` is set to, the result is always a whole
/// // number of records and at least one record wide.
/// let record_bytes = 256;
/// let target = training_read_target_bytes_from_env(record_bytes);
/// assert_eq!(target % record_bytes, 0);
/// assert!(target >= record_bytes);
/// ```
pub fn training_read_target_bytes_from_env(record_bytes: usize) -> usize {
    let rb = record_bytes.max(1);
    let env = std::env::var("NEAT_SCORER_READ_BYTES").ok();
    let (parsed, warning) = crate::env_tuning::parse_tuning_var(
        "NEAT_SCORER_READ_BYTES",
        env.as_deref(),
        DEFAULT_READ_BYTES,
        |s| s.parse::<usize>().ok(),
    );
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }
    let raw = parsed.clamp(rb, MAX_READ_BYTES);
    (raw / rb) * rb
}

/// Stable label for the active training-read implementation (for JSON diagnostics).
///
/// # Examples
///
/// ```
/// use rust_scorer::read_tuning::training_read_backend_label;
///
/// // `native_pipelined` on native targets, `wasm_chunked` under wasm32.
/// let label = training_read_backend_label();
/// assert!(label == "native_pipelined" || label == "wasm_chunked");
/// ```
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
