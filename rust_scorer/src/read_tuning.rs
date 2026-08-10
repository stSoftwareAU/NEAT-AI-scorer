//! `NEAT_SCORER_READ_BYTES` tuning for chunked `.bin` reads (aligned to whole records).
//!
//! `neat_core::training_bin_stream` exposes a single [`for_each_read_chunk`] path; buffer sizing
//! stays configurable here so callers can widen reads for parallel activation batches.
//!
//! Defaults are **record-size adaptive** (Issue #504) and **host-RAM adaptive**
//! ([`crate::host_resources`]): old machines keep a small chunk while mid-range
//! and large Macs take the production buffer.
//!
//! [`for_each_read_chunk`]: neat_core::training_bin_stream::for_each_read_chunk

use crate::host_resources::{self, GIB, HostResources};

const DEFAULT_READ_BYTES: usize = 2 * 1024 * 1024;

/// Production-scale records (~2461 inputs + 1 output ≈ 9848 bytes/record) amortise
/// poorly at the 2 MiB default — each read yields only ~213 records and one
/// GPU dispatch per chunk. Issue #307 / production tuning: when the env var is
/// unset, use a larger default so omit/`auto` callers get fewer chunks.
const LARGE_RECORD_BYTES_THRESHOLD: usize = 8000;
const LARGE_RECORD_DEFAULT_READ_BYTES: usize = 32 * 1024 * 1024;

/// Mid-host upper bound for read buffer size (matches previous `neat_core` tuner
/// cap). Large Macs (≥ 64 GiB RAM) may clamp higher via [`max_read_bytes`].
pub(crate) const MAX_READ_BYTES: usize = 64 * 1024 * 1024;

/// Host-aware upper clamp for `NEAT_SCORER_READ_BYTES`.
#[must_use]
pub(crate) fn max_read_bytes() -> usize {
    host_resources::max_read_bytes(&host_resources::host())
}

/// Default read target when `NEAT_SCORER_READ_BYTES` is unset.
pub(crate) fn default_training_read_bytes_for(record_bytes: usize, host: &HostResources) -> usize {
    let rb = record_bytes.max(1);
    let large_record = rb >= LARGE_RECORD_BYTES_THRESHOLD;
    let desired = if large_record {
        match host.physical_ram_bytes {
            // Very large Macs: take the full mid-host cap by default.
            Some(ram) if ram >= 64 * GIB => MAX_READ_BYTES,
            _ => LARGE_RECORD_DEFAULT_READ_BYTES,
        }
    } else {
        DEFAULT_READ_BYTES
    };

    // RAM ceiling: never ask an old machine for the production 32 MiB default.
    let ram_cap = match host.physical_ram_bytes {
        Some(ram) if ram < 4 * GIB => DEFAULT_READ_BYTES,
        Some(ram) if ram < 8 * GIB => 8 * 1024 * 1024,
        Some(ram) if ram < 16 * GIB => 16 * 1024 * 1024,
        // Unknown RAM: do not invent a tighter cap than the record-size default.
        None => desired,
        Some(_) => host_resources::max_read_bytes(host),
    };

    desired.min(ram_cap).max(rb)
}

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
    let host = host_resources::host();
    let default = default_training_read_bytes_for(rb, &host);
    let (parsed, warning) = crate::env_tuning::parse_tuning_var(
        "NEAT_SCORER_READ_BYTES",
        env.as_deref(),
        default,
        |s| s.parse::<usize>().ok(),
    );
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }
    let raw = parsed.clamp(rb, host_resources::max_read_bytes(&host));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_resources::{GIB, HostResources};

    #[test]
    fn default_read_bytes_scales_for_production_records_on_mid_host() {
        let mid = HostResources::synthetic(10, Some(24 * GIB));
        assert_eq!(
            default_training_read_bytes_for(256, &mid),
            DEFAULT_READ_BYTES
        );
        assert_eq!(
            default_training_read_bytes_for(LARGE_RECORD_BYTES_THRESHOLD, &mid),
            LARGE_RECORD_DEFAULT_READ_BYTES
        );
        assert_eq!(
            default_training_read_bytes_for(9848, &mid),
            LARGE_RECORD_DEFAULT_READ_BYTES
        );
    }

    #[test]
    fn default_read_bytes_shrinks_on_low_ram() {
        let old = HostResources::synthetic(4, Some(3 * GIB));
        assert_eq!(
            default_training_read_bytes_for(9848, &old),
            DEFAULT_READ_BYTES
        );
    }

    #[test]
    fn ram_cap_uses_snapped_ram() {
        // 16 GB nameplate x86 Linux host: the probe reports 15.5 GiB.
        // Both the read cap here and the worker ceiling in `host_resources`
        // must tier as a 16 GiB host, or one call site has bypassed the
        // central snap (Issue #547).
        let probed = HostResources::synthetic(8, Some(16_642_998_272));
        let nameplate = HostResources::synthetic(8, Some(16 * GIB));

        assert_eq!(
            default_training_read_bytes_for(9848, &probed),
            LARGE_RECORD_DEFAULT_READ_BYTES
        );
        assert_eq!(
            default_training_read_bytes_for(9848, &probed),
            default_training_read_bytes_for(9848, &nameplate)
        );
        assert_eq!(
            host_resources::max_worker_count(&probed),
            host_resources::max_worker_count(&nameplate)
        );
    }

    #[test]
    fn low_ram_read_cap_still_applies_below_the_nameplate_band() {
        // 7 GiB is too far below 8 GiB to be a reservation artefact, so the
        // 8 MiB low-RAM cap must survive the snap.
        let small = HostResources::synthetic(8, Some(7 * GIB));
        assert_eq!(
            default_training_read_bytes_for(9848, &small),
            8 * 1024 * 1024
        );
    }

    #[test]
    fn large_mac_takes_full_mid_host_cap_for_production_records() {
        let big = HostResources::synthetic(32, Some(128 * GIB));
        assert_eq!(default_training_read_bytes_for(9848, &big), MAX_READ_BYTES);
        assert_eq!(host_resources::max_read_bytes(&big), 256 * 1024 * 1024);
    }
}
