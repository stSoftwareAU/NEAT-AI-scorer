//! Host capability probe for self-tuning across old machines and large Macs.
//!
//! The scorer picks thread counts, read-chunk sizes and GPU scratch budgets from
//! the machine it is running on — not from a single production-host constant —
//! so a low-RAM box stays within memory and a high-core Mac is not capped at
//! the historical 64-worker ceiling.
//!
//! Override knobs (`NEAT_SCORER_READ_BYTES`, `NEAT_SCORER_ACTIVATION_THREADS`,
//! …) still win when set; this module only supplies the **defaults** and the
//! clamp ceilings those defaults / overrides share.

use std::sync::OnceLock;

/// 2 GiB.
pub(crate) const GIB: u64 = 1024 * 1024 * 1024;

/// Snapshot of the host the process is running on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostResources {
    /// Logical CPUs from [`std::thread::available_parallelism`] (at least 1).
    pub cpus: usize,
    /// Physical RAM in bytes when the platform probe succeeds.
    pub physical_ram_bytes: Option<u64>,
}

impl HostResources {
    /// Build a synthetic host (unit tests / deterministic policy checks).
    #[must_use]
    pub const fn synthetic(cpus: usize, physical_ram_bytes: Option<u64>) -> Self {
        Self {
            cpus: if cpus == 0 { 1 } else { cpus },
            physical_ram_bytes,
        }
    }

    /// Probe the real host once per process.
    #[must_use]
    pub fn probe() -> Self {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .max(1);
        Self {
            cpus,
            physical_ram_bytes: physical_memory_bytes(),
        }
    }
}

/// Process-wide host probe (lazy, read-only after init).
#[must_use]
pub fn host() -> HostResources {
    static HOST: OnceLock<HostResources> = OnceLock::new();
    *HOST.get_or_init(HostResources::probe)
}

/// Absolute ceiling on activation / file-reader workers for this host.
///
/// Historical constant was 64 — enough for M-series laptops, but it left
/// ultra / studio Macs under-subscribed. Low-RAM hosts keep a tight cap so a
/// typo'd `NEAT_SCORER_ACTIVATION_THREADS=999` cannot spawn more clones than
/// the machine can hold.
#[must_use]
pub fn max_worker_count(host: &HostResources) -> usize {
    match host.physical_ram_bytes {
        Some(ram) if ram < 2 * GIB => 2,
        Some(ram) if ram < 4 * GIB => 4,
        Some(ram) if ram < 8 * GIB => 16,
        // Unknown RAM: keep the historical 64 ceiling (safe mid-range).
        None => 64,
        // ≥ 8 GiB: allow large Macs up to 256 logical workers.
        Some(_) => 256,
    }
}

/// Default Rayon / file-reader worker count when the matching env var is unset.
///
/// Uses every logical CPU on mid and large hosts; on low-RAM machines it
/// clamps below `available_parallelism` so compiled-network clones and read
/// buffers cannot dominate physical memory.
#[must_use]
pub fn default_worker_count(host: &HostResources) -> usize {
    host.cpus.max(1).min(max_worker_count(host))
}

/// Upper clamp for `NEAT_SCORER_READ_BYTES` on this host.
///
/// Mid-range hosts keep the historical 64 MiB ceiling; Macs with ≥ 64 GiB RAM
/// may raise an override as high as 256 MiB. Read-chunk *defaults* are chosen
/// in [`crate::read_tuning`] (record-size + RAM adaptive).
#[must_use]
pub fn max_read_bytes(host: &HostResources) -> usize {
    const LEGACY_MAX: usize = 64 * 1024 * 1024;
    const LARGE_MAC_MAX: usize = 256 * 1024 * 1024;
    match host.physical_ram_bytes {
        Some(ram) if ram >= 64 * GIB => LARGE_MAC_MAX,
        _ => LEGACY_MAX,
    }
}

/// Default GPU scratch SSBO budget (bytes) for `forward_mse_scratch`.
///
/// Scales with physical RAM so a 4 GiB host is not asked for a 512 MiB scratch
/// buffer, while a 64 GiB+ Mac can host a larger grid.
#[must_use]
pub fn default_gpu_scratch_bytes(host: &HostResources) -> u64 {
    const MIB: u64 = 1024 * 1024;
    match host.physical_ram_bytes {
        Some(ram) if ram < 4 * GIB => 64 * MIB,
        Some(ram) if ram < 8 * GIB => 128 * MIB,
        Some(ram) if ram < 16 * GIB => 256 * MIB,
        Some(ram) if ram >= 64 * GIB => 1024 * MIB,
        // Mid-range and unknown: historical 512 MiB default (Issue #182).
        Some(_) | None => 512 * MIB,
    }
}

/// Physical memory in bytes, or `None` when the platform probe is unavailable.
fn physical_memory_bytes() -> Option<u64> {
    physical_memory_bytes_impl()
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "ios"
))]
fn physical_memory_bytes_impl() -> Option<u64> {
    // SAFETY: `sysconf` with `_SC_PHYS_PAGES` / `_SC_PAGESIZE` is the portable
    // POSIX probe; both return -1 on failure which we map to `None`.
    unsafe {
        let pages = libc::sysconf(libc::_SC_PHYS_PAGES);
        let page_size = libc::sysconf(libc::_SC_PAGESIZE);
        if pages > 0 && page_size > 0 {
            Some((pages as u64).saturating_mul(page_size as u64))
        } else {
            None
        }
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "ios"
)))]
fn physical_memory_bytes_impl() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_ram_host_caps_workers_and_scratch() {
        let old = HostResources::synthetic(8, Some(3 * GIB));
        assert_eq!(max_worker_count(&old), 4);
        assert_eq!(default_worker_count(&old), 4);
        assert_eq!(default_gpu_scratch_bytes(&old), 64 * 1024 * 1024);
        assert_eq!(max_read_bytes(&old), 64 * 1024 * 1024);
    }

    #[test]
    fn mid_host_keeps_historical_scratch_and_raises_worker_ceiling() {
        let mid = HostResources::synthetic(10, Some(24 * GIB));
        assert_eq!(max_worker_count(&mid), 256);
        assert_eq!(default_worker_count(&mid), 10);
        assert_eq!(default_gpu_scratch_bytes(&mid), 512 * 1024 * 1024);
        assert_eq!(max_read_bytes(&mid), 64 * 1024 * 1024);
    }

    #[test]
    fn large_mac_raises_read_and_scratch_ceilings() {
        let big = HostResources::synthetic(32, Some(128 * GIB));
        assert_eq!(default_worker_count(&big), 32);
        assert_eq!(max_worker_count(&big), 256);
        assert_eq!(max_read_bytes(&big), 256 * 1024 * 1024);
        assert_eq!(default_gpu_scratch_bytes(&big), 1024 * 1024 * 1024);
    }

    #[test]
    fn unknown_ram_preserves_historical_mid_defaults() {
        let unknown = HostResources::synthetic(12, None);
        assert_eq!(max_worker_count(&unknown), 64);
        assert_eq!(default_worker_count(&unknown), 12);
        assert_eq!(max_read_bytes(&unknown), 64 * 1024 * 1024);
        assert_eq!(default_gpu_scratch_bytes(&unknown), 512 * 1024 * 1024);
    }

    #[test]
    fn probe_returns_at_least_one_cpu() {
        let h = HostResources::probe();
        assert!(h.cpus >= 1);
    }
}
