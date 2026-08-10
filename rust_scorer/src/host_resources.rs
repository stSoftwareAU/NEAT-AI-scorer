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
//!
//! Every RAM tier compares against [`HostResources::physical_ram_bytes`], which
//! is the probe **snapped to the host's nameplate capacity**
//! ([`snap_to_nameplate_bytes`], Issue #547) — the raw POSIX probe reports
//! usable memory, so an x86 Linux box sold as 16 GB reads ≈ 15.5 GiB and would
//! otherwise drop a whole tier.

use std::sync::OnceLock;

use crate::gpu::GpuBackendLabel;

/// 2 GiB.
pub(crate) const GIB: u64 = 1024 * 1024 * 1024;

/// 1 MiB.
const MIB: u64 = 1024 * 1024;

/// What the selected `wgpu` adapter can actually do (Issue #548).
///
/// Sensed once per process from the adapter `gpu::select_adapter` already
/// creates — never by a probe of its own — so a `--gpu off` run and a
/// GPU-less host both stay at zero adapter cost. `None` on
/// [`HostResources::gpu`] means "no adapter sensed": either none exists, or
/// nothing on this run has asked for one yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuCapability {
    /// Native backend hosting the adapter. Never
    /// [`GpuBackendLabel::CpuFallback`] — a non-native backend is reported as
    /// "no adapter" by `gpu::select_adapter`, so it never reaches here.
    pub backend: GpuBackendLabel,
    /// Whether adapter memory is **unified** with system RAM (Apple silicon,
    /// integrated GPUs) rather than separate VRAM (discrete cards). A unified
    /// scratch allocation competes with the corpus for the same DRAM, so its
    /// budget stays bounded by physical RAM.
    pub unified_memory: bool,
    /// `wgpu::Limits::max_storage_buffer_binding_size`, in bytes. The scratch
    /// activation SSBO is a single binding, so this is a hard ceiling on the
    /// scratch budget — exceeding it yields a validation error, not a slow run.
    pub max_storage_buffer_binding_size: u64,
    /// `wgpu::Limits::max_compute_workgroups_per_dimension`. Bounds the
    /// grid-stride width `G_x` the scratch kernel may dispatch, whatever the
    /// memory budget allows.
    pub max_compute_workgroups_per_dimension: u32,
}

impl GpuCapability {
    /// Build a capability snapshot (also used by policy tests to pin a
    /// synthetic adapter).
    #[must_use]
    pub const fn new(
        backend: GpuBackendLabel,
        unified_memory: bool,
        max_storage_buffer_binding_size: u64,
        max_compute_workgroups_per_dimension: u32,
    ) -> Self {
        Self {
            backend,
            unified_memory,
            max_storage_buffer_binding_size,
            max_compute_workgroups_per_dimension,
        }
    }
}

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
    /// Physical RAM in bytes when the platform probe succeeds, snapped to the
    /// host's **nameplate** capacity by [`snap_to_nameplate_bytes`] (Issue
    /// #547) so every tier below compares against the memory the box was sold
    /// with, not the slightly smaller figure the kernel leaves usable.
    pub physical_ram_bytes: Option<u64>,
    /// Capability of the GPU adapter sensed for this process (Issue #548), or
    /// `None` when no adapter has been sensed — a GPU-less host, a `--gpu off`
    /// run, or any run that has not yet needed an adapter. `None` keeps every
    /// GPU knob on its pre-#548 RAM-derived value.
    pub gpu: Option<GpuCapability>,
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
    /// `performance_cpus` is clamped into `[1, cpus]`, and
    /// `physical_ram_bytes` goes through [`snap_to_nameplate_bytes`] exactly as
    /// a real probe does, so a synthetic host tiers identically to the machine
    /// whose reading it reproduces.
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
            // Single construction point, so the nameplate snap (Issue #547)
            // cannot be bypassed.
            physical_ram_bytes: match physical_ram_bytes {
                Some(raw) => Some(snap_to_nameplate_bytes(raw)),
                None => None,
            },
            // Sensing an adapter costs a `wgpu` initialisation, so a synthetic
            // host starts with none; pin one with [`Self::with_gpu`].
            gpu: None,
        }
    }

    /// Pin a synthetic GPU capability on this host (Issue #548), so policy
    /// tests can reproduce an adapter tier without owning one.
    #[must_use]
    pub const fn with_gpu(self, gpu: GpuCapability) -> Self {
        Self {
            cpus: self.cpus,
            performance_cpus: self.performance_cpus,
            physical_ram_bytes: self.physical_ram_bytes,
            gpu: Some(gpu),
        }
    }

    /// Probe the real host once per process.
    ///
    /// CPU and RAM are probed here; the GPU capability is whatever
    /// [`sensed_gpu_capability`] holds — this **never** creates an adapter of
    /// its own (Issue #548), so `--gpu off` and GPU-less hosts pay nothing.
    #[must_use]
    pub fn probe() -> Self {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .max(1);
        let host = Self::synthetic_with_performance_cpus(
            cpus,
            resolve_performance_cpus(cpus, performance_core_count()),
            physical_memory_bytes(),
        );
        match sensed_gpu_capability() {
            Some(gpu) => host.with_gpu(gpu),
            None => host,
        }
    }
}

/// GPU capability sensed for this process, or `None` when no adapter has been
/// sensed yet (Issue #548).
///
/// Populated exactly once, by `gpu::select_adapter`, from the adapter that call
/// already creates. Reading it never creates one.
#[must_use]
pub fn sensed_gpu_capability() -> Option<GpuCapability> {
    SENSED_GPU.get().copied()
}

/// Cache the capability of the adapter `gpu::select_adapter` just selected.
///
/// First writer wins: a process only ever selects one adapter, and later calls
/// re-select the same one. Deliberately **not** a probe — nothing here can
/// create an adapter, so a `--gpu off` run leaves the cell empty.
pub(crate) fn record_gpu_capability(gpu: GpuCapability) {
    let _ = SENSED_GPU.set(gpu);
}

/// Process-wide sensed GPU capability (lazy, write-once).
static SENSED_GPU: OnceLock<GpuCapability> = OnceLock::new();

/// Apply the P-core fallback chain: a missing, zero or oversized probe result
/// falls back to the logical CPU count — never fewer (Issue #546).
fn resolve_performance_cpus(cpus: usize, probed: Option<usize>) -> usize {
    let cpus = cpus.max(1);
    probed.filter(|&n| n > 0).unwrap_or(cpus).clamp(1, cpus)
}

/// Nameplate RAM capacities, in GiB, a probed reading may be snapped up to.
///
/// Powers of two plus the common 1.5× and Apple-specific configurations, in
/// ascending order — [`snap_to_nameplate_bytes`] relies on the ordering.
const NAMEPLATE_CAPACITY_GIB: [u64; 23] = [
    1, 2, 3, 4, 6, 8, 12, 16, 18, 24, 32, 36, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536,
];

/// How far below a nameplate capacity a reading may sit and still count as
/// that capacity: one sixteenth, i.e. 6.25 %.
///
/// Observed x86-Linux shortfalls are 3.75 % (15.4 of 16 GiB) to 5.0 % (7.6 of
/// 8 GiB); 6.25 % covers them with headroom while leaving a genuinely smaller
/// machine — 7 GiB against an 8 GiB capacity, 12.5 % short — in its own tier.
const NAMEPLATE_TOLERANCE_DIVISOR: u64 = 16;

/// Round a probed RAM reading up to the host's nameplate capacity.
///
/// `sysconf(_SC_PHYS_PAGES) * sysconf(_SC_PAGESIZE)` reports **usable** memory,
/// so firmware and kernel reservations put a nominally 16 GB x86 Linux box a
/// few hundred MiB below `16 * GIB` and every strict `<` tier below silently
/// drops it a whole tier (Issue #547). A reading within **6.25 %** of the next
/// nameplate capacity (powers of two plus the common 1.5× and Apple-specific
/// sizes) is treated as that capacity; anything further below is left exactly
/// as probed, and the result is never lower than the input.
///
/// # Examples
///
/// ```
/// use rust_scorer::host_resources::snap_to_nameplate_bytes;
///
/// const GIB: u64 = 1024 * 1024 * 1024;
/// // A 16 GB x86 Linux box reporting 15.5 GiB tiers as 16 GiB …
/// assert_eq!(snap_to_nameplate_bytes(16_642_998_272), 16 * GIB);
/// // … while an exact Apple Silicon reading is untouched …
/// assert_eq!(snap_to_nameplate_bytes(24 * GIB), 24 * GIB);
/// // … and a genuinely smaller machine keeps its own reading.
/// assert_eq!(snap_to_nameplate_bytes(7 * GIB), 7 * GIB);
/// ```
#[must_use]
pub const fn snap_to_nameplate_bytes(probed_bytes: u64) -> u64 {
    let mut i = 0;
    while i < NAMEPLATE_CAPACITY_GIB.len() {
        let capacity = NAMEPLATE_CAPACITY_GIB[i] * GIB;
        if probed_bytes >= capacity {
            i += 1;
            continue;
        }
        // First capacity above the reading — the only candidate to snap to.
        if capacity - probed_bytes <= capacity / NAMEPLATE_TOLERANCE_DIVISOR {
            return capacity;
        }
        return probed_bytes;
    }
    probed_bytes
}

/// Process-wide host probe (lazy, read-only after init).
///
/// The CPU/RAM probe runs once; the GPU capability is overlaid on every read
/// (Issue #548) because an adapter is normally sensed *after* the first knob
/// has already been resolved — a cached `None` would pin every later GPU knob
/// to its no-adapter value.
#[must_use]
pub fn host() -> HostResources {
    static HOST: OnceLock<HostResources> = OnceLock::new();
    let host = *HOST.get_or_init(HostResources::probe);
    match sensed_gpu_capability() {
        Some(gpu) => host.with_gpu(gpu),
        None => host,
    }
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

/// Upper clamp for `NEAT_SCORER_READ_BYTES` on this host: the historical 64 MiB
/// ceiling, on **every** host.
///
/// **Issue #549 removed the `≥ 64 GiB → 256 MiB` tier.** No built-in default
/// could select it — `read_tuning::default_training_read_bytes_for_readers`
/// tops out at `MAX_READ_BYTES` (64 MiB) — so a 256 MiB read was reachable only
/// by hand-setting the env var, which Issue #544 rules out as a configuration
/// mechanism. With the Issue #529 reader count now bounding the *aggregate*
/// `readers × chunk` footprint, a 256 MiB chunk needs 16 GiB of RAM per reader
/// (384 GiB on a 24-core M2 Ultra) — no fleet host qualifies, so the ceiling is
/// dropped to what the defaults can actually select rather than raised.
///
/// The parameter is retained so every knob resolver keeps taking its host
/// snapshot from one place; the answer no longer varies by host. Read-chunk
/// *defaults* are chosen in [`crate::read_tuning`] (record-size, RAM and
/// reader-count adaptive).
#[must_use]
pub fn max_read_bytes(_host: &HostResources) -> usize {
    crate::read_tuning::MAX_READ_BYTES
}

/// Share of unified system RAM a scratch grid may claim: one sixteenth
/// (6.25 %).
///
/// On a shared-memory host the scratch SSBO and the streamed corpus live in the
/// same DRAM, so the budget has to leave the read chunks (32 MiB each) and the
/// compiled creature pool room. Every shipped RAM tier already sits below this
/// share — it is the guard that keeps a *future* tier from asking a small Mac
/// for a corpus-evicting buffer.
const UNIFIED_RAM_SHARE_DIVISOR: u64 = 16;

/// Share of a **discrete** adapter's storage-buffer binding limit a scratch
/// grid may claim: one quarter.
///
/// Discrete VRAM is not system RAM, so a RAM tier describes nothing about the
/// card; a quarter of what it can bind leaves the record, neuron, synapse and
/// partial SSBOs their share of the device.
const DISCRETE_BINDING_SHARE_DIVISOR: u64 = 4;

/// Default GPU scratch SSBO budget (bytes) for `forward_mse_scratch`.
///
/// The RAM tiering below is the starting point on every host. With an adapter
/// sensed (Issue #548) that figure is then **tightened** by what the adapter
/// reports — its binding-size limit, and on a discrete card its own share of
/// that limit — and floored to a power of two. Sensing never *raises* the
/// budget: the Apple M4 Pro A/B in `docs/performance-baseline.md` measured a
/// doubled budget **7.9 % slower** on the shallow scratch path, so the retune
/// half of Issue #548 is a recorded negative result and only the clamp ships.
///
/// With no adapter sensed (GPU-less host, `--gpu off`, or a knob resolved
/// before any adapter exists) the pre-#548 RAM-only answer is returned
/// unchanged.
///
/// `NEAT_SCORER_GPU_SCRATCH_BYTES` overrides either answer — see
/// `crate::gpu::forward_mse_batched::scratch_budget_bytes_from_env`.
#[must_use]
pub fn default_gpu_scratch_bytes(host: &HostResources) -> u64 {
    match host.gpu {
        Some(gpu) => sensed_gpu_scratch_bytes(host, &gpu),
        None => ram_derived_gpu_scratch_bytes(host),
    }
}

/// Scratch budget for a host with a **sensed** adapter (Issue #548).
///
/// Never exceeds the adapter's reported binding-size limit — the scratch
/// activations are one binding, so a budget above it is a validation error
/// rather than a slow run — and is floored to a power of two so the runner's
/// `next_power_of_two` buffer allocation cannot round back above that limit.
/// Every bound here is a `min`, so a sensed host can only ever get a *smaller*
/// budget than the same host with no adapter sensed.
fn sensed_gpu_scratch_bytes(host: &HostResources, gpu: &GpuCapability) -> u64 {
    let mut budget = ram_derived_gpu_scratch_bytes(host);
    if gpu.unified_memory {
        // Shared DRAM: the scratch buffer competes with the streamed corpus.
        if let Some(ram) = host.physical_ram_bytes {
            budget = budget.min(ram / UNIFIED_RAM_SHARE_DIVISOR);
        }
    } else {
        // Discrete VRAM: host RAM describes nothing, so bound by the card.
        budget = budget.min(gpu.max_storage_buffer_binding_size / DISCRETE_BINDING_SHARE_DIVISOR);
    }
    // The hard clamp: a device that binds less than the tier wants wins.
    budget = budget.min(gpu.max_storage_buffer_binding_size.max(64));
    floor_power_of_two(budget)
}

/// Pre-#548 RAM-only tiering, kept verbatim as the no-adapter answer.
///
/// Scales with physical RAM so a 4 GiB host is not asked for a 512 MiB scratch
/// buffer, while a 64 GiB+ Mac can host a larger grid.
fn ram_derived_gpu_scratch_bytes(host: &HostResources) -> u64 {
    match host.physical_ram_bytes {
        Some(ram) if ram < 4 * GIB => 64 * MIB,
        Some(ram) if ram < 8 * GIB => 128 * MIB,
        Some(ram) if ram < 16 * GIB => 256 * MIB,
        Some(ram) if ram >= 64 * GIB => 1024 * MIB,
        // Mid-range and unknown: historical 512 MiB default (Issue #182).
        Some(_) | None => 512 * MIB,
    }
}

/// Largest power of two at or below `bytes` (`0` only for `0`).
const fn floor_power_of_two(bytes: u64) -> u64 {
    if bytes == 0 {
        return 0;
    }
    1 << (u64::BITS - 1 - bytes.leading_zeros())
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
    fn large_mac_raises_the_scratch_ceiling_and_keeps_the_flat_read_ceiling() {
        let big = HostResources::synthetic(32, Some(128 * GIB));
        assert_eq!(default_worker_count(&big), 32);
        assert_eq!(max_worker_count(&big), 256);
        // Issue #549: was 256 MiB, a ceiling no built-in default could select.
        assert_eq!(max_read_bytes(&big), 64 * 1024 * 1024);
        assert_eq!(default_gpu_scratch_bytes(&big), 1024 * 1024 * 1024);
    }

    #[test]
    fn read_ceiling_no_longer_tiers_on_ram() {
        // Issue #549: the read ceiling is flat, so it has no tier that a
        // built-in default cannot reach. Reintroducing a RAM tier here fails
        // both this test and `read_tuning::tests::no_unreachable_ceiling`.
        for ram in [
            Some(GIB),
            Some(3 * GIB),
            Some(8 * GIB),
            Some(16 * GIB),
            Some(24 * GIB),
            Some(64 * GIB),
            Some(192 * GIB),
            Some(1536 * GIB),
            None,
        ] {
            let host = HostResources::synthetic(12, ram);
            assert_eq!(
                max_read_bytes(&host),
                64 * 1024 * 1024,
                "read ceiling must not tier on RAM ({ram:?})"
            );
        }
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

    // --- Nameplate snapping (Issue #547) ---------------------------------
    //
    // Real x86-Linux readings from the fleet health dashboard: `sysconf` reports
    // *usable* memory, so firmware/kernel reservations put a nominally 8 or
    // 16 GB box a few hundred MiB below the exact power-of-two tier boundary.
    // Page-aligned (4 KiB) so they are values a real probe could return.

    /// 8 GB nameplate x86 Linux host: probe reports ≈ 7.6 GiB.
    const PROBED_8GB_X86_BYTES: u64 = 8_160_436_224;
    /// 16 GB nameplate x86 Linux host: probe reports ≈ 15.4 GiB.
    const PROBED_16GB_X86_BYTES_LOW: u64 = 16_535_621_632;
    /// 16 GB nameplate x86 Linux host: probe reports 15.5 GiB.
    const PROBED_16GB_X86_BYTES_HIGH: u64 = 16_642_998_272;

    #[test]
    fn snaps_7_6_gib_to_8_gib_tier() {
        let host = HostResources::synthetic(8, Some(PROBED_8GB_X86_BYTES));
        assert_eq!(host.physical_ram_bytes, Some(8 * GIB));
        // Was the low-RAM 16-worker cap and a 128 MiB scratch budget.
        assert_eq!(max_worker_count(&host), 256);
        assert_eq!(default_worker_count(&host), 8);
        assert_eq!(default_gpu_scratch_bytes(&host), 256 * 1024 * 1024);
        assert_eq!(max_read_bytes(&host), 64 * 1024 * 1024);
    }

    #[test]
    fn snaps_15_4_and_15_5_gib_to_16_gib_tier() {
        for probed in [PROBED_16GB_X86_BYTES_LOW, PROBED_16GB_X86_BYTES_HIGH] {
            let host = HostResources::synthetic(8, Some(probed));
            assert_eq!(host.physical_ram_bytes, Some(16 * GIB));
            assert_eq!(max_worker_count(&host), 256);
            // Was the < 16 GiB 256 MiB scratch tier.
            assert_eq!(default_gpu_scratch_bytes(&host), 512 * 1024 * 1024);
            assert_eq!(max_read_bytes(&host), 64 * 1024 * 1024);
        }
    }

    #[test]
    fn exact_24_gib_and_64_gib_unchanged() {
        let m4_pro = HostResources::synthetic(10, Some(24 * GIB));
        assert_eq!(m4_pro.physical_ram_bytes, Some(24 * GIB));
        assert_eq!(max_worker_count(&m4_pro), 256);
        assert_eq!(default_gpu_scratch_bytes(&m4_pro), 512 * 1024 * 1024);
        assert_eq!(max_read_bytes(&m4_pro), 64 * 1024 * 1024);

        let big = HostResources::synthetic(32, Some(64 * GIB));
        assert_eq!(big.physical_ram_bytes, Some(64 * GIB));
        // Issue #549: flat 64 MiB ceiling — the 256 MiB tier was unreachable.
        assert_eq!(max_read_bytes(&big), 64 * 1024 * 1024);
        assert_eq!(default_gpu_scratch_bytes(&big), 1024 * 1024 * 1024);
    }

    #[test]
    fn apple_silicon_reported_values_are_unchanged() {
        // `hw.memsize` is already the nameplate figure on every shipped
        // configuration, so snapping must be a no-op there.
        for gib in [8_u64, 16, 18, 24, 32, 36, 48, 64, 96, 128, 192, 512] {
            assert_eq!(snap_to_nameplate_bytes(gib * GIB), gib * GIB);
        }
    }

    #[test]
    fn none_probe_unchanged() {
        let unknown = HostResources::synthetic(12, None);
        assert_eq!(unknown.physical_ram_bytes, None);
        assert_eq!(max_worker_count(&unknown), 64);
        assert_eq!(default_worker_count(&unknown), 12);
        assert_eq!(max_read_bytes(&unknown), 64 * 1024 * 1024);
        assert_eq!(default_gpu_scratch_bytes(&unknown), 512 * 1024 * 1024);
    }

    #[test]
    fn genuinely_small_host_is_not_snapped_up_a_tier() {
        // 7.0 GiB is 12.5 % below the 8 GiB nameplate — far more than
        // firmware reservations explain, so it stays in the low-RAM tier.
        let small = HostResources::synthetic(8, Some(7 * GIB));
        assert_eq!(small.physical_ram_bytes, Some(7 * GIB));
        assert_eq!(max_worker_count(&small), 16);
        assert_eq!(default_gpu_scratch_bytes(&small), 128 * 1024 * 1024);

        let tiny = HostResources::synthetic(4, Some(3 * GIB));
        assert_eq!(tiny.physical_ram_bytes, Some(3 * GIB));
        assert_eq!(max_worker_count(&tiny), 4);
    }

    #[test]
    fn snapping_never_lowers_a_reading() {
        for bytes in [0, 1, GIB / 2, 5 * GIB, 7 * GIB, 100 * GIB, 4096 * GIB] {
            assert!(snap_to_nameplate_bytes(bytes) >= bytes);
        }
    }

    // --- GPU capability sensing (Issue #548) ------------------------------

    /// `max_storage_buffer_binding_size` every shipped Apple silicon adapter
    /// reports through `wgpu` (4 GiB − 4, the saturated `u32` limit).
    const APPLE_BINDING_LIMIT: u64 = 4_294_967_292;
    /// `max_compute_workgroups_per_dimension` on Metal and Vulkan.
    const FLEET_MAX_WORKGROUPS: u32 = 65_535;

    /// An Apple silicon (unified memory, Metal) adapter.
    const fn apple_gpu() -> GpuCapability {
        GpuCapability::new(
            GpuBackendLabel::Metal,
            true,
            APPLE_BINDING_LIMIT,
            FLEET_MAX_WORKGROUPS,
        )
    }

    /// A discrete card with its own VRAM (Vulkan / Dx12 host).
    const fn discrete_gpu(binding_limit: u64) -> GpuCapability {
        GpuCapability::new(
            GpuBackendLabel::Vulkan,
            false,
            binding_limit,
            FLEET_MAX_WORKGROUPS,
        )
    }

    #[test]
    fn synthetic_can_pin_a_gpu_capability() {
        let host = HostResources::synthetic(12, Some(24 * GIB)).with_gpu(apple_gpu());
        assert_eq!(host.gpu, Some(apple_gpu()));
        assert_eq!(host.gpu.expect("pinned").backend, GpuBackendLabel::Metal);
        assert!(host.gpu.expect("pinned").unified_memory);
        // Pinning a GPU must not disturb anything else the host senses.
        assert_eq!(host.cpus, 12);
        assert_eq!(host.physical_ram_bytes, Some(24 * GIB));
        assert_eq!(HostResources::synthetic(12, Some(24 * GIB)).gpu, None);
    }

    #[test]
    fn a_host_with_no_sensed_adapter_keeps_the_pre_548_scratch_budget() {
        // The GPU-less x86 Linux boxes and every `--gpu off` run land here.
        for (cpus, ram) in [
            (8, Some(3 * GIB)),
            (8, Some(8 * GIB)),
            (12, Some(24 * GIB)),
            (24, Some(64 * GIB)),
            (12, None),
        ] {
            let host = HostResources::synthetic(cpus, ram);
            assert_eq!(host.gpu, None);
            assert_eq!(
                default_gpu_scratch_bytes(&host),
                ram_derived_gpu_scratch_bytes(&host),
                "no adapter must behave exactly as before Issue #548 ({ram:?})"
            );
        }
    }

    #[test]
    fn sensed_budget_never_exceeds_the_adapter_binding_limit() {
        // A device that binds only 128 MiB (wgpu's conservative default) caps
        // the budget however much RAM the host has.
        let limited = GpuCapability::new(GpuBackendLabel::Vulkan, true, 128 * MIB, 65_535);
        let big_ram = HostResources::synthetic(24, Some(128 * GIB)).with_gpu(limited);
        assert_eq!(default_gpu_scratch_bytes(&big_ram), 128 * MIB);

        // Even a limit below the lowest RAM tier wins — an unbindable buffer
        // is a validation error, not a slow run.
        let tiny = GpuCapability::new(GpuBackendLabel::Gl, true, 16 * MIB, 65_535);
        let host = HostResources::synthetic(8, Some(64 * GIB)).with_gpu(tiny);
        assert_eq!(default_gpu_scratch_bytes(&host), 16 * MIB);

        for ram in [Some(3 * GIB), Some(24 * GIB), Some(192 * GIB), None] {
            for gpu in [apple_gpu(), discrete_gpu(256 * MIB), discrete_gpu(u64::MAX)] {
                let host = HostResources::synthetic(12, ram).with_gpu(gpu);
                assert!(
                    default_gpu_scratch_bytes(&host) <= gpu.max_storage_buffer_binding_size,
                    "budget must fit the {} B binding limit (ram {ram:?})",
                    gpu.max_storage_buffer_binding_size
                );
            }
        }
    }

    #[test]
    fn sensing_an_apple_adapter_keeps_every_fleet_tier_on_its_ram_budget() {
        // Issue #548 negative result: doubling the budget measured 7.9 % slower
        // on the M4 Pro shallow scratch path, so sensing must not raise it.
        // Apple silicon binds 4 GiB on every tier, so nothing tightens either.
        for (cpus, ram_gib, expected_mib) in [
            (8, 8, 256),    // M1 / M1 Pro, 8 GB
            (10, 16, 512),  // M1 Pro, 16 GB
            (12, 24, 512),  // M4 Pro / M4 / M1 Max fleet hosts, 24 GB
            (10, 32, 512),  // M1 Max, 32 GB
            (24, 64, 1024), // M2 Ultra, 64 GB
        ] {
            let host = HostResources::synthetic(cpus, Some(ram_gib * GIB)).with_gpu(apple_gpu());
            let flat = HostResources::synthetic(cpus, Some(ram_gib * GIB));
            assert_eq!(
                default_gpu_scratch_bytes(&host),
                expected_mib * MIB,
                "{ram_gib} GiB Apple tier"
            );
            assert_eq!(
                default_gpu_scratch_bytes(&host),
                default_gpu_scratch_bytes(&flat),
                "{ram_gib} GiB: sensing an Apple adapter must not move the budget"
            );
        }
    }

    #[test]
    fn a_sensed_adapter_never_raises_the_budget() {
        // Every bound in the sensed path is a `min`, so no adapter/RAM
        // combination can hand a host more scratch than the RAM tier alone.
        for ram in [
            Some(GIB),
            Some(3 * GIB),
            Some(8 * GIB),
            Some(24 * GIB),
            Some(192 * GIB),
            None,
        ] {
            let flat = HostResources::synthetic(12, ram);
            for gpu in [
                apple_gpu(),
                discrete_gpu(128 * MIB),
                discrete_gpu(4 * GIB),
                discrete_gpu(u64::MAX),
            ] {
                assert!(
                    default_gpu_scratch_bytes(&flat.with_gpu(gpu))
                        <= default_gpu_scratch_bytes(&flat),
                    "sensing may only tighten the budget (ram {ram:?})"
                );
            }
        }
    }

    #[test]
    fn unified_memory_hosts_stay_bounded_by_system_ram() {
        for ram_gib in [1_u64, 3, 8, 24, 64, 192] {
            let host = HostResources::synthetic(12, Some(ram_gib * GIB)).with_gpu(apple_gpu());
            assert!(
                default_gpu_scratch_bytes(&host) <= (ram_gib * GIB) / UNIFIED_RAM_SHARE_DIVISOR,
                "{ram_gib} GiB shared-memory host must not get a corpus-evicting buffer"
            );
        }
    }

    #[test]
    fn a_unified_host_with_unknown_ram_keeps_the_historical_budget() {
        // No RAM figure bounds nothing extra — the historical default stands.
        let unknown = HostResources::synthetic(12, None).with_gpu(apple_gpu());
        assert_eq!(default_gpu_scratch_bytes(&unknown), 512 * MIB);
    }

    #[test]
    fn a_discrete_adapter_is_bounded_by_its_own_binding_limit() {
        // Host RAM describes nothing about VRAM, so a big-RAM host with a small
        // card is bounded by the card, not by its 1 GiB RAM tier.
        let big_host_small_card =
            HostResources::synthetic(24, Some(128 * GIB)).with_gpu(discrete_gpu(512 * MIB));
        assert_eq!(default_gpu_scratch_bytes(&big_host_small_card), 128 * MIB);
        // A card with room to spare leaves the RAM tier alone.
        let big_host_big_card =
            HostResources::synthetic(24, Some(128 * GIB)).with_gpu(discrete_gpu(16 * GIB));
        assert_eq!(default_gpu_scratch_bytes(&big_host_big_card), 1024 * MIB);
    }

    #[test]
    fn every_sensed_budget_is_a_usable_power_of_two() {
        // `BatchedRunner::ensure_scratch_buf` rounds its allocation up to a
        // power of two, so a non-power-of-two budget could round back above the
        // binding limit.
        for ram in [
            Some(GIB),
            Some(3 * GIB),
            Some(24 * GIB),
            Some(96 * GIB),
            None,
        ] {
            for gpu in [
                apple_gpu(),
                discrete_gpu(768 * MIB),
                discrete_gpu(3 * GIB),
                GpuCapability::new(GpuBackendLabel::Dx12, true, 100 * MIB, 65_535),
            ] {
                let host = HostResources::synthetic(12, ram).with_gpu(gpu);
                let budget = default_gpu_scratch_bytes(&host);
                assert!(budget > 0, "a sensed budget is always positive");
                assert!(
                    budget.is_power_of_two(),
                    "budget {budget} for ram {ram:?} must be a power of two"
                );
            }
        }
    }

    #[test]
    fn floor_power_of_two_rounds_down() {
        assert_eq!(floor_power_of_two(0), 0);
        assert_eq!(floor_power_of_two(1), 1);
        assert_eq!(floor_power_of_two(1023), 512);
        assert_eq!(floor_power_of_two(1024), 1024);
        assert_eq!(floor_power_of_two(1536 * MIB), 1024 * MIB);
        assert_eq!(floor_power_of_two(u64::MAX), 1 << 63);
    }

    #[test]
    fn probing_a_host_creates_no_adapter() {
        // Sensing rides `gpu::select_adapter`; the host probe must never reach
        // for `wgpu` itself, or `--gpu off` would pay for a GPU it disabled.
        let probed = HostResources::probe();
        assert_eq!(probed.gpu, sensed_gpu_capability());
    }

    // --- Fleet tier table (Issue #550) ------------------------------------

    /// Production record width: 2461 `f32` inputs + 1 output.
    const PRODUCTION_RECORD_BYTES: usize = 9848;

    /// Every row of the **Fleet tier table** in `docs/self-tuning.md`, as
    /// `(tier, logical CPUs, probed RAM, workers, per-reader read chunk,
    /// aggregate read budget, no-adapter GPU scratch)`.
    ///
    /// The read chunk is the production-width chunk **one** reader takes when
    /// the corpus has at least as many shards as the host has CPUs (production
    /// ships 26), so the reader count is the worker default.
    const FLEET_TIER_TABLE: [(&str, usize, u64, usize, usize, usize, u64); 11] = [
        ("Apple 8-core, 8 GB", 8, 8 * GIB, 8, 8_380_648, 64, 256),
        ("Apple 8-core, 16 GB", 8, 16 * GIB, 8, 8_380_648, 64, 512),
        ("Apple 10-core, 16 GB", 10, 16 * GIB, 10, 6_706_488, 64, 512),
        ("Apple 10-core, 24 GB", 10, 24 * GIB, 10, 6_706_488, 64, 512),
        ("Apple 10-core, 32 GB", 10, 32 * GIB, 10, 6_706_488, 64, 512),
        ("Apple 12-core, 24 GB", 12, 24 * GIB, 12, 5_583_816, 64, 512),
        (
            "Apple 24-core, 64 GB",
            24,
            64 * GIB,
            24,
            11_177_480,
            256,
            1024,
        ),
        (
            "x86 Linux 4-core, 8 GB",
            4,
            PROBED_8GB_X86_BYTES,
            4,
            16_771_144,
            64,
            256,
        ),
        (
            "x86 Linux 4-core, 16 GB",
            4,
            PROBED_16GB_X86_BYTES_LOW,
            4,
            16_771_144,
            64,
            512,
        ),
        (
            "x86 Linux 8-core, 16 GB",
            8,
            PROBED_16GB_X86_BYTES_HIGH,
            8,
            8_380_648,
            64,
            512,
        ),
        (
            "x86 Linux 12-core, 16 GB",
            12,
            PROBED_16GB_X86_BYTES_LOW,
            12,
            5_583_816,
            64,
            512,
        ),
    ];

    #[test]
    fn every_fleet_tier_resolves_the_documented_knobs() {
        // The tier table is documentation with teeth: `docs/self-tuning.md`
        // publishes these exact numbers, so a knob resolver that moves a tier
        // fails here before the doc can silently drift.
        for (tier, cpus, ram, workers, chunk, aggregate_mib, scratch_mib) in FLEET_TIER_TABLE {
            let host = HostResources::synthetic(cpus, Some(ram));
            let readers = default_worker_count(&host);
            assert_eq!(readers, workers, "{tier}: worker / reader default");
            assert_eq!(max_worker_count(&host), 256, "{tier}: worker ceiling");
            assert_eq!(
                max_read_bytes(&host),
                64 * 1024 * 1024,
                "{tier}: read ceiling"
            );
            assert_eq!(
                crate::read_tuning::aggregate_read_budget_bytes(&host),
                aggregate_mib * 1024 * 1024,
                "{tier}: aggregate read budget"
            );
            let raw = crate::read_tuning::default_training_read_bytes_for_readers(
                PRODUCTION_RECORD_BYTES,
                &host,
                readers,
            );
            // The public resolver rounds down to whole records; the tier table
            // publishes that aligned figure, as `--host-report` prints it.
            let aligned = (raw / PRODUCTION_RECORD_BYTES) * PRODUCTION_RECORD_BYTES;
            assert_eq!(aligned, chunk, "{tier}: per-reader read chunk");
            assert!(
                readers * aligned <= crate::read_tuning::aggregate_read_budget_bytes(&host),
                "{tier}: readers x chunk must fit the aggregate budget"
            );
            assert_eq!(
                default_gpu_scratch_bytes(&host),
                scratch_mib * MIB,
                "{tier}: no-adapter GPU scratch budget"
            );
        }
    }

    #[test]
    fn sensing_an_apple_adapter_leaves_every_fleet_tier_row_intact() {
        // The fleet's Macs all sense a unified-memory Metal adapter; the doc
        // publishes one scratch column, so sensing must not split it in two.
        for (tier, cpus, ram, _, _, _, scratch_mib) in FLEET_TIER_TABLE {
            if !tier.starts_with("Apple") {
                continue;
            }
            let sensed = HostResources::synthetic(cpus, Some(ram)).with_gpu(apple_gpu());
            assert_eq!(
                default_gpu_scratch_bytes(&sensed),
                scratch_mib * MIB,
                "{tier}: a sensed Apple adapter must not move the documented budget"
            );
        }
    }
}
