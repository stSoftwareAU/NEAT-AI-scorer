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
    /// Performance ("P") cores on a heterogeneous host (Issue #546).
    ///
    /// Apple silicon reports both performance and efficiency cores in
    /// [`std::thread::available_parallelism`], but the E-cores are far slower
    /// at the sustained SIMD/unpack work the scoring pools do — and the fused
    /// path fork/joins per chunk, so an E-core straggler gates the barrier.
    /// Equal to [`Self::cpus`] wherever the platform exposes no split (x86,
    /// Intel Macs, any probe failure) — **never fewer**, so a failed probe
    /// cannot starve parallelism.
    pub performance_cpus: usize,
    /// Physical RAM in bytes when the platform probe succeeds.
    pub physical_ram_bytes: Option<u64>,
}

impl HostResources {
    /// Build a synthetic host with no P/E split (unit tests / deterministic
    /// policy checks): every logical CPU is a performance core.
    #[must_use]
    pub const fn synthetic(cpus: usize, physical_ram_bytes: Option<u64>) -> Self {
        Self::synthetic_with_performance_cpus(cpus, cpus, physical_ram_bytes)
    }

    /// Build a synthetic host with a pinned performance/efficiency core split
    /// (Issue #546), so policy tests can reproduce a fleet tier exactly —
    /// e.g. `synthetic_with_performance_cpus(12, 8, …)` for an M4 Pro (8P+4E).
    ///
    /// `performance_cpus` is clamped into `[1, cpus]`.
    #[must_use]
    pub const fn synthetic_with_performance_cpus(
        cpus: usize,
        performance_cpus: usize,
        physical_ram_bytes: Option<u64>,
    ) -> Self {
        let cpus = if cpus == 0 { 1 } else { cpus };
        let performance_cpus = if performance_cpus == 0 {
            1
        } else if performance_cpus > cpus {
            cpus
        } else {
            performance_cpus
        };
        Self {
            cpus,
            performance_cpus,
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
            performance_cpus: resolve_performance_cpus(cpus, performance_core_count()),
            physical_ram_bytes: physical_memory_bytes(),
        }
    }
}

/// Apply the P-core fallback chain: a missing, zero or oversized probe result
/// falls back to the logical CPU count — never fewer (Issue #546).
fn resolve_performance_cpus(cpus: usize, probed: Option<usize>) -> usize {
    let cpus = cpus.max(1);
    probed.filter(|&n| n > 0).unwrap_or(cpus).clamp(1, cpus)
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
///
/// **Issue #546 — still keyed off [`HostResources::cpus`], not
/// [`HostResources::performance_cpus`].** The P/E split is now probed and
/// reported, but keying the default off the P-core count is a *performance*
/// change and this project only ships one with before/after evidence (see the
/// [Performance Task Workflow](../../CONTRIBUTING.md#performance-task-workflow)).
/// The attempted A/B and why it is inconclusive are recorded in the Issue #546
/// section of `docs/performance-baseline.md`; until a quiescent multi-tier
/// capture exists, every host keeps its historical worker count.
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

/// Performance-core count, or `None` when this platform exposes no P/E split
/// (Issue #546). `None` means "no data" — the caller keeps the logical count.
fn performance_core_count() -> Option<usize> {
    performance_core_count_impl()
}

/// macOS: Apple silicon publishes `hw.perflevel0.physicalcpu` (performance
/// tier) and `hw.perflevel1.physicalcpu` (efficiency tier). Intel Macs and
/// older kernels publish neither, so fall back to `hw.physicalcpu` before
/// giving up.
#[cfg(target_os = "macos")]
fn performance_core_count_impl() -> Option<usize> {
    sysctl_positive_int(c"hw.perflevel0.physicalcpu")
        .or_else(|| sysctl_positive_int(c"hw.physicalcpu"))
}

/// Read one `int32` `hw.*` sysctl, or `None` when the key is absent or
/// non-positive.
#[cfg(target_os = "macos")]
fn sysctl_positive_int(name: &std::ffi::CStr) -> Option<usize> {
    let mut value: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    // SAFETY: `name` is NUL-terminated; `value` and `len` describe a correctly
    // sized destination for these `int32` keys, and no new value is written
    // (null `newp`, zero `newlen`). A non-zero return means "key absent".
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::addr_of_mut!(value).cast(),
            std::ptr::addr_of_mut!(len),
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    usize::try_from(value).ok().filter(|&n| n > 0)
}

/// Linux: heterogeneous (big.LITTLE / DynamIQ) hosts expose a per-CPU
/// `cpu_capacity`; the highest capacity is the performance tier. x86 boxes
/// publish no `cpu_capacity` at all, so this returns `None` there and the
/// logical count stands — those hosts are unchanged by Issue #546.
#[cfg(target_os = "linux")]
fn performance_core_count_impl() -> Option<usize> {
    let mut capacities: Vec<u64> = Vec::new();
    for entry in std::fs::read_dir("/sys/devices/system/cpu").ok()?.flatten() {
        let Ok(raw) = std::fs::read_to_string(entry.path().join("cpu_capacity")) else {
            continue;
        };
        if let Ok(capacity) = raw.trim().parse::<u64>() {
            capacities.push(capacity);
        }
    }
    let max = capacities.iter().copied().max()?;
    let count = capacities.iter().filter(|&&c| c == max).count();
    (count > 0).then_some(count)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn performance_core_count_impl() -> Option<usize> {
    None
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

    // --- Issue #546: performance-core probe ---------------------------------

    #[test]
    fn probe_reports_performance_cores_within_the_logical_count() {
        let h = HostResources::probe();
        assert!(h.performance_cpus >= 1, "at least one performance core");
        assert!(
            h.performance_cpus <= h.cpus,
            "performance cores ({}) cannot exceed logical CPUs ({})",
            h.performance_cpus,
            h.cpus
        );
    }

    #[test]
    fn failed_performance_probe_falls_back_to_every_logical_cpu() {
        // The whole fallback chain (`hw.perflevel0.physicalcpu` →
        // `hw.physicalcpu` → logical count) ends here: no data must never
        // resolve to *fewer* workers than before Issue #546.
        assert_eq!(resolve_performance_cpus(12, None), 12);
        assert_eq!(resolve_performance_cpus(1, None), 1);
        // A probe that answers zero is no data either.
        assert_eq!(resolve_performance_cpus(12, Some(0)), 12);
    }

    #[test]
    fn performance_probe_result_is_clamped_into_the_logical_range() {
        assert_eq!(resolve_performance_cpus(12, Some(8)), 8);
        // A nonsensical over-count cannot inflate the worker default.
        assert_eq!(resolve_performance_cpus(12, Some(99)), 12);
    }

    #[test]
    fn synthetic_without_a_split_treats_every_cpu_as_a_performance_core() {
        let h = HostResources::synthetic(12, Some(24 * GIB));
        assert_eq!(h.performance_cpus, h.cpus);
    }

    /// The fleet tiers Issue #546 names, as `(logical, performance, RAM)`.
    const FLEET_TIERS: [(usize, usize, u64); 4] = [
        (12, 8, 24),  // M4 Pro — 8P + 4E
        (10, 4, 24),  // M4 — 4P + 6E
        (24, 16, 64), // M2 Ultra — 16P + 8E
        (8, 8, 16),   // x86 Linux / Intel Mac — no P/E split probed
    ];

    #[test]
    fn shipped_worker_default_is_unchanged_by_the_performance_core_split() {
        // Issue #546 ships the probe but **not** a retune: the P/E split must
        // not move any host's worker count until a multi-tier benchmark
        // capture justifies it. A retune landing here without that evidence
        // fails this test.
        for (cpus, performance_cpus, ram_gib) in FLEET_TIERS {
            let ram = Some(ram_gib * GIB);
            let split = HostResources::synthetic_with_performance_cpus(cpus, performance_cpus, ram);
            let flat = HostResources::synthetic(cpus, ram);
            assert_eq!(
                default_worker_count(&split),
                default_worker_count(&flat),
                "tier {cpus}L/{performance_cpus}P: the P/E split must not move the worker default"
            );
            assert_eq!(
                default_worker_count(&split),
                cpus.min(max_worker_count(&split)),
                "tier {cpus}L/{performance_cpus}P: worker default is the logical count under the ceiling"
            );
        }
    }

    #[test]
    fn a_host_with_no_performance_core_data_never_loses_workers() {
        // The fallback chain (`hw.perflevel0.physicalcpu` → `hw.physicalcpu` →
        // logical count) ends at the logical count, so an unclassifiable host
        // keeps exactly its pre-#546 default.
        for (cpus, _, ram_gib) in FLEET_TIERS {
            let ram = Some(ram_gib * GIB);
            let no_data = HostResources::synthetic(cpus, ram);
            assert_eq!(no_data.performance_cpus, cpus);
            assert!(
                default_worker_count(&no_data) >= 1,
                "every host keeps at least one worker"
            );
            assert_eq!(
                default_worker_count(&no_data),
                cpus.min(max_worker_count(&no_data))
            );
        }
    }

    #[test]
    fn worker_default_still_clamps_to_the_host_ceiling_on_a_split_host() {
        // A low-RAM heterogeneous host is capped by RAM, not by either core
        // count — the ceiling policy is untouched by Issue #546.
        let low_ram = HostResources::synthetic_with_performance_cpus(12, 8, Some(3 * GIB));
        assert_eq!(max_worker_count(&low_ram), 4);
        assert_eq!(default_worker_count(&low_ram), 4);
    }

    #[test]
    fn synthetic_clamps_a_pinned_performance_core_split() {
        let over = HostResources::synthetic_with_performance_cpus(8, 99, None);
        assert_eq!(over.performance_cpus, 8);
        let zero = HostResources::synthetic_with_performance_cpus(8, 0, None);
        assert_eq!(zero.performance_cpus, 1);
        let zero_cpus = HostResources::synthetic_with_performance_cpus(0, 4, None);
        assert_eq!(zero_cpus.cpus, 1);
        assert_eq!(zero_cpus.performance_cpus, 1);
    }
}
