//! `NEAT_SCORER_READ_BYTES` tuning for chunked `.bin` reads (aligned to whole records).
//!
//! `neat_core::training_bin_stream` exposes a single [`for_each_read_chunk`] path; buffer sizing
//! stays configurable here so callers can widen reads for parallel activation batches.
//!
//! Defaults are **record-size adaptive** (Issue #504), **host-RAM adaptive**
//! ([`crate::host_resources`]) and **reader-count aware** (Issue #549): old
//! machines keep a small chunk, mid-range and large Macs take the production
//! buffer, and every default is bounded so the aggregate `readers × chunk`
//! footprint of the Issue #529 concurrent readers fits the host's aggregate read
//! budget (`aggregate_read_budget_bytes`).
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

/// Upper clamp for a hand-set `NEAT_SCORER_READ_BYTES`, on **every** host
/// (matches the previous `neat_core` tuner cap).
///
/// Issue #549 removed the old `≥ 64 GiB → 256 MiB` clamp tier. No built-in
/// default could select it — the record-size tiers top out here — so as an
/// *override ceiling* it was reachable only by hand-setting the env var, which
/// Issue #544 rules out as a configuration mechanism. The 256 MiB figure it
/// really described is the **aggregate** read budget of a large-RAM host, which
/// now lives in [`aggregate_read_budget_bytes`] where the defaults reach it.
pub(crate) const MAX_READ_BYTES: usize = 64 * 1024 * 1024;

/// Total resident read buffer every concurrent reader may hold **together** on a
/// host with < 64 GiB RAM.
///
/// Issue #529 gives the fused path one reader per CPU, so peak resident read
/// buffer is `readers × chunk`, not one chunk. That total has been bounded since
/// #529 (`stream_score::per_reader_read_buf_len` divided one budget across the
/// readers) — but the budget it divided was [`MAX_READ_BYTES`], the *override
/// clamp*, so `read_tuning` chose a per-reader default that the reader split
/// then silently overrode. Issue #549 names the budget, keeps its shipped value,
/// and feeds it to the default so the chosen chunk is the chunk the readers get.
const AGGREGATE_READ_BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// Aggregate read budget on a host with ≥ 64 GiB RAM (Mac Studio / Mac Pro
/// class): four times [`AGGREGATE_READ_BUDGET_BYTES`].
///
/// This is the value the removed `max_read_bytes` tier was actually supplying to
/// the #529 reader split, so keeping it here leaves every ≥ 64 GiB host's
/// resident read buffer exactly where it shipped.
const LARGE_RAM_AGGREGATE_READ_BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// Hard share of physical RAM the aggregate read budget may never exceed: one
/// sixteenth (6.25 %), matching the GPU scratch share in
/// [`crate::host_resources`].
///
/// The tiers above are already inside it on every fleet host — 64 MiB is 0.4 %
/// of a 16 GiB M4 — so this is the guard that keeps a *future* tier (or a tiny
/// host below 1 GiB) from asking for a corpus-evicting read buffer.
const AGGREGATE_READ_RAM_SHARE_DIVISOR: u64 = 16;

/// Host-aware upper clamp for `NEAT_SCORER_READ_BYTES`.
#[must_use]
pub(crate) fn max_read_bytes() -> usize {
    host_resources::max_read_bytes(&host_resources::host())
}

/// Total read buffer **all** concurrent readers on this host may hold at once
/// (Issue #549).
///
/// RAM-tiered, then hard-bounded by [`AGGREGATE_READ_RAM_SHARE_DIVISOR`]. An
/// unknown-RAM host keeps the mid-range tier, exactly as every other knob does.
#[must_use]
pub(crate) fn aggregate_read_budget_bytes(host: &HostResources) -> usize {
    let tier = match host.physical_ram_bytes {
        Some(ram) if ram >= 64 * GIB => LARGE_RAM_AGGREGATE_READ_BUDGET_BYTES,
        _ => AGGREGATE_READ_BUDGET_BYTES,
    };
    match host.physical_ram_bytes {
        Some(ram) => {
            tier.min(usize::try_from(ram / AGGREGATE_READ_RAM_SHARE_DIVISOR).unwrap_or(usize::MAX))
        }
        None => tier,
    }
}

/// Per-reader share of [`aggregate_read_budget_bytes`] for `readers` concurrent
/// readers (Issue #549).
///
/// A *budget*, not the answer: the caller still takes the smaller of it and the
/// record-size / RAM tier, and never goes below one whole record.
#[must_use]
pub(crate) fn per_reader_read_budget_bytes(host: &HostResources, readers: usize) -> usize {
    aggregate_read_budget_bytes(host) / readers.max(1)
}

/// Default read target when `NEAT_SCORER_READ_BYTES` is unset, for `readers`
/// concurrent `.bin` readers (Issue #549).
///
/// Three bounds, smallest wins, and the result is never narrower than one whole
/// record:
///
/// 1. the record-size tier — production-width records amortise poorly at the
///    2 MiB small-record default (Issue #504/#307);
/// 2. the host-RAM ceiling — never ask an old machine for the 32 MiB default;
/// 3. the per-reader share of the aggregate read budget
///    ([`per_reader_read_budget_bytes`]) — with one reader per CPU (Issue #529)
///    the resident read buffer is `readers × chunk`.
///
/// Bound 3 is what `stream_score::per_reader_read_buf_len` has applied to the
/// resolved buffer since Issue #529; applying it here too means the default this
/// function returns *is* the buffer each reader gets, so `--host-report` and the
/// JSON diagnostics stop overstating it. Every fleet tier keeps the same
/// resident buffer it shipped with.
pub(crate) fn default_training_read_bytes_for_readers(
    record_bytes: usize,
    host: &HostResources,
    readers: usize,
) -> usize {
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

    let chosen = desired
        .min(ram_cap)
        .min(per_reader_read_budget_bytes(host, readers));
    // A buffer narrower than one record can never complete a read.
    chosen.max(rb)
}

/// Target bytes per `read` for a **single sequential** reader (rounded down to a
/// multiple of `record_bytes`).
///
/// Concurrent readers (Issue #529) must use
/// [`training_read_target_bytes_from_env_for_readers`] so the aggregate
/// `readers × chunk` footprint stays bounded (Issue #549).
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
    training_read_target_bytes_from_env_for_readers(record_bytes, 1)
}

/// Target bytes per `read` for each of `readers` concurrent `.bin` readers
/// (Issue #549), rounded down to a multiple of `record_bytes`.
///
/// The default is bounded so `readers × result` stays inside this host's
/// aggregate read budget; `NEAT_SCORER_READ_BYTES` still overrides it and is
/// still clamped to `[record_bytes, max_read_bytes]` and record-aligned exactly
/// as before — an operator who sets the knob by hand gets what they asked for.
///
/// # Examples
///
/// ```
/// use rust_scorer::read_tuning::training_read_target_bytes_from_env_for_readers;
///
/// let record_bytes = 9848;
/// // More readers can only narrow each reader's chunk, never widen it …
/// let one = training_read_target_bytes_from_env_for_readers(record_bytes, 1);
/// let many = training_read_target_bytes_from_env_for_readers(record_bytes, 64);
/// assert!(many <= one);
/// // … and every answer is still a whole number of records.
/// assert_eq!(many % record_bytes, 0);
/// assert!(many >= record_bytes);
/// ```
pub fn training_read_target_bytes_from_env_for_readers(
    record_bytes: usize,
    readers: usize,
) -> usize {
    let rb = record_bytes.max(1);
    let env = std::env::var("NEAT_SCORER_READ_BYTES").ok();
    let host = host_resources::host();
    let default = default_training_read_bytes_for_readers(rb, &host, readers);
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

    /// The record-size / RAM tier tests below describe the **single sequential
    /// reader** default: with concurrent readers the aggregate budget splits it
    /// (Issue #549), which the `FLEET_TIERS` tests cover.
    const SINGLE_READER: usize = 1;

    #[test]
    fn default_read_bytes_scales_for_production_records_on_mid_host() {
        let mid = HostResources::synthetic(10, Some(24 * GIB));
        assert_eq!(
            default_training_read_bytes_for_readers(256, &mid, SINGLE_READER),
            DEFAULT_READ_BYTES
        );
        assert_eq!(
            default_training_read_bytes_for_readers(
                LARGE_RECORD_BYTES_THRESHOLD,
                &mid,
                SINGLE_READER
            ),
            LARGE_RECORD_DEFAULT_READ_BYTES
        );
        assert_eq!(
            default_training_read_bytes_for_readers(9848, &mid, SINGLE_READER),
            LARGE_RECORD_DEFAULT_READ_BYTES
        );
    }

    #[test]
    fn default_read_bytes_shrinks_on_low_ram() {
        let old = HostResources::synthetic(4, Some(3 * GIB));
        assert_eq!(
            default_training_read_bytes_for_readers(9848, &old, SINGLE_READER),
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
            default_training_read_bytes_for_readers(9848, &probed, SINGLE_READER),
            LARGE_RECORD_DEFAULT_READ_BYTES
        );
        assert_eq!(
            default_training_read_bytes_for_readers(9848, &probed, SINGLE_READER),
            default_training_read_bytes_for_readers(9848, &nameplate, SINGLE_READER)
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
            default_training_read_bytes_for_readers(9848, &small, SINGLE_READER),
            8 * 1024 * 1024
        );
    }

    #[test]
    fn large_mac_takes_full_mid_host_cap_for_production_records() {
        let big = HostResources::synthetic(32, Some(128 * GIB));
        assert_eq!(
            default_training_read_bytes_for_readers(9848, &big, SINGLE_READER),
            MAX_READ_BYTES
        );
        // Issue #549: the read ceiling no longer tiers on RAM — the old
        // `≥ 64 GiB → 256 MiB` branch was selectable by no built-in default.
        assert_eq!(host_resources::max_read_bytes(&big), MAX_READ_BYTES);
    }

    // --- Issue #549: fleet tiers and the aggregate reader footprint ---------

    /// Production splits its corpus across 26 `.bin` shards (Issue #529), so
    /// that is the file count the shipped reader default is capped at.
    const PRODUCTION_SHARD_COUNT: usize = 26;

    /// Production record width: 2461 inputs + 1 output, `f32`.
    const PRODUCTION_RECORD_BYTES: usize = 9848;

    /// Real fleet tiers as `(label, logical cpus, RAM GiB)`.
    ///
    /// Mirrors the tiers Issue #549 names — every Apple host from an 8 GB M1 to
    /// the single M2 Ultra, plus the x86 Linux control and the 10-core/16 GB M4
    /// the aggregate-footprint regression was found on.
    const FLEET_TIERS: [(&str, usize, u64); 8] = [
        ("M1 8 GB", 8, 8),
        ("M4 16 GB", 10, 16),
        ("x86 Linux 16 GB", 8, 16),
        ("M4 24 GB", 10, 24),
        ("M4 Pro 24 GB", 12, 24),
        ("M1 Max 32 GB", 10, 32),
        ("M2 Ultra 64 GB", 24, 64),
        ("M2 Ultra 192 GB", 24, 192),
    ];

    /// The reader count the shipped default resolves to on a 26-shard corpus:
    /// [`host_resources::default_worker_count`] capped at the file count
    /// (`stream_score::file_read_worker_count_for`, with the env unset).
    fn shipped_readers(host: &HostResources) -> usize {
        host_resources::default_worker_count(host).min(PRODUCTION_SHARD_COUNT)
    }

    #[test]
    fn shipped_reader_count_matches_the_fused_path_resolver() {
        // The footprint bound below is only meaningful if it is fed the reader
        // count the fused path actually spawns.
        for (label, cpus, ram_gib) in FLEET_TIERS {
            let host = HostResources::synthetic(cpus, Some(ram_gib * GIB));
            assert_eq!(
                shipped_readers(&host),
                crate::stream_score::file_read_worker_count_for(PRODUCTION_SHARD_COUNT, &host),
                "{label}: reader count used by the footprint bound must be the shipped one"
            );
        }
    }

    #[test]
    fn no_unreachable_ceiling() {
        // Every distinct value `host_resources::max_read_bytes` can return must
        // be selectable by a built-in default on some real host — a ceiling no
        // default can reach is configurable only by hand-setting the env var,
        // which Issue #544 rules out. The dead `≥ 64 GiB → 256 MiB` branch
        // Issue #549 removed fails this test if it is reintroduced.
        let mut hosts: Vec<(String, HostResources)> = FLEET_TIERS
            .iter()
            .map(|&(label, cpus, ram_gib)| {
                (
                    label.to_string(),
                    HostResources::synthetic(cpus, Some(ram_gib * GIB)),
                )
            })
            .collect();
        // Plus a wide synthetic grid, so a reintroduced high tier cannot hide
        // behind "no fleet host has that much RAM".
        for ram_gib in [1_u64, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 128, 512, 1536] {
            for cpus in [1_usize, 2, 4, 8, 10, 12, 16, 24, 32, 64] {
                hosts.push((
                    format!("synthetic {cpus}c/{ram_gib} GiB"),
                    HostResources::synthetic(cpus, Some(ram_gib * GIB)),
                ));
            }
        }
        hosts.push((
            "unknown RAM".to_string(),
            HostResources::synthetic(12, None),
        ));

        let mut ceilings: Vec<usize> = hosts
            .iter()
            .map(|(_, host)| host_resources::max_read_bytes(host))
            .collect();
        ceilings.sort_unstable();
        ceilings.dedup();

        for ceiling in ceilings {
            let reached = hosts.iter().any(|(_, host)| {
                for record_bytes in [40, PRODUCTION_RECORD_BYTES] {
                    for readers in [1, shipped_readers(host)] {
                        if default_training_read_bytes_for_readers(record_bytes, host, readers)
                            == ceiling
                        {
                            return true;
                        }
                    }
                }
                false
            });
            assert!(
                reached,
                "read ceiling {ceiling} B is selectable by no built-in default on any host — \
                 raise a default to use it or remove the ceiling (Issue #549)"
            );
        }
    }

    #[test]
    fn aggregate_footprint_bounded() {
        // `readers × chunk` is the peak resident read buffer with one reader per
        // CPU (Issue #529), and it must fit the host's aggregate read budget —
        // which is itself inside a sane fraction of RAM. The specific regression
        // Issue #549 names — a 10-core 16 GiB M4 asking for 10 × 32 MiB =
        // 320 MiB — is the `M4 16 GB` row.
        for (label, cpus, ram_gib) in FLEET_TIERS {
            let host = HostResources::synthetic(cpus, Some(ram_gib * GIB));
            let ram = ram_gib * GIB;
            let budget = aggregate_read_budget_bytes(&host) as u64;
            assert!(
                budget <= ram / AGGREGATE_READ_RAM_SHARE_DIVISOR,
                "{label}: a {budget} B read budget is more than 1/{AGGREGATE_READ_RAM_SHARE_DIVISOR} \
                 of {ram} B of RAM"
            );
            let readers = shipped_readers(&host);
            for record_bytes in [40, PRODUCTION_RECORD_BYTES] {
                let chunk =
                    default_training_read_bytes_for_readers(record_bytes, &host, readers) as u64;
                let footprint = chunk * readers as u64;
                assert!(
                    footprint <= budget,
                    "{label}: {readers} readers × {chunk} B = {footprint} B of read buffer \
                     exceeds the {budget} B budget ({record_bytes} B records)"
                );
            }
        }
        // And on a wide synthetic grid, so the bound is not fitted to the fleet.
        for ram_gib in [1_u64, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 128, 512, 1536] {
            for cpus in [1_usize, 2, 4, 8, 10, 12, 16, 24, 32, 64, 256] {
                let host = HostResources::synthetic(cpus, Some(ram_gib * GIB));
                let readers = shipped_readers(&host);
                let budget = aggregate_read_budget_bytes(&host) as u64;
                let chunk = default_training_read_bytes_for_readers(
                    PRODUCTION_RECORD_BYTES,
                    &host,
                    readers,
                ) as u64;
                let footprint = chunk * readers as u64;
                // The one-record floor may exceed the budget on an absurdly
                // wide reader count; a read narrower than a record cannot work.
                assert!(
                    footprint <= budget || chunk == PRODUCTION_RECORD_BYTES as u64,
                    "{cpus}c/{ram_gib} GiB: {readers} × {chunk} B exceeds {budget} B"
                );
            }
        }
    }

    #[test]
    fn the_16_gib_m4_regression_input_is_now_bounded() {
        // Documents the exact input Issue #549 was raised on: unbounded, the
        // 10-core 16 GiB M4 took the full 32 MiB per reader.
        let m4 = HostResources::synthetic(10, Some(16 * GIB));
        let readers = shipped_readers(&m4);
        assert_eq!(readers, 10, "one reader per CPU on a 26-shard corpus");
        let budget = aggregate_read_budget_bytes(&m4) as u64;

        // The pre-#549 answer — the record-size tier, sized per reader in
        // isolation — overshoots the budget.
        let unbounded = LARGE_RECORD_DEFAULT_READ_BYTES as u64 * readers as u64;
        assert!(
            unbounded > budget,
            "the regression input must overshoot: {unbounded} B vs {budget} B"
        );

        let chunk = default_training_read_bytes_for_readers(PRODUCTION_RECORD_BYTES, &m4, readers);
        assert!(
            chunk < LARGE_RECORD_DEFAULT_READ_BYTES,
            "the bound must narrow this tier's chunk, got {chunk} B"
        );
        // The bound is tight: the chunk is the per-reader share of the budget,
        // less only the record-alignment remainder.
        let footprint = chunk as u64 * readers as u64;
        assert!(
            footprint <= budget && footprint + readers as u64 > budget,
            "{readers} × {chunk} B = {footprint} B must be the tight fit under {budget} B"
        );
    }

    #[test]
    fn a_single_reader_keeps_the_full_record_size_tier() {
        // The continuous single-stream sweep holds one buffer, so the
        // reader-count bound must not touch it — the pre-#549 behaviour: the
        // record-size tier under the host-RAM cap (16 MiB below 16 GiB).
        for (label, cpus, ram_gib) in FLEET_TIERS {
            let host = HostResources::synthetic(cpus, Some(ram_gib * GIB));
            let expected = match ram_gib {
                ram if ram >= 64 => MAX_READ_BYTES,
                ram if ram >= 16 => LARGE_RECORD_DEFAULT_READ_BYTES,
                _ => 16 * 1024 * 1024,
            };
            assert_eq!(
                default_training_read_bytes_for_readers(PRODUCTION_RECORD_BYTES, &host, 1),
                expected,
                "{label}: a single sequential reader keeps its record-size tier"
            );
        }
    }

    #[test]
    fn more_readers_never_widen_the_chunk() {
        for (label, cpus, ram_gib) in FLEET_TIERS {
            let host = HostResources::synthetic(cpus, Some(ram_gib * GIB));
            let mut previous = usize::MAX;
            for readers in [1_usize, 2, 4, 8, 10, 12, 24, 26, 64, 256] {
                let chunk = default_training_read_bytes_for_readers(
                    PRODUCTION_RECORD_BYTES,
                    &host,
                    readers,
                );
                assert!(
                    chunk <= previous,
                    "{label}: {readers} readers widened the chunk to {chunk} B"
                );
                assert!(
                    chunk >= PRODUCTION_RECORD_BYTES,
                    "{label}: {readers} readers must still hold one whole record"
                );
                previous = chunk;
            }
        }
    }

    #[test]
    fn unknown_ram_keeps_the_mid_range_budget_at_any_reader_count() {
        // No RAM figure means no RAM-derived tightening: the host keeps the
        // mid-range aggregate budget, exactly as every other knob does.
        let unknown = HostResources::synthetic(12, None);
        assert_eq!(
            aggregate_read_budget_bytes(&unknown),
            AGGREGATE_READ_BUDGET_BYTES
        );
        // One reader takes the whole record-size tier …
        assert_eq!(
            default_training_read_bytes_for_readers(PRODUCTION_RECORD_BYTES, &unknown, 1),
            LARGE_RECORD_DEFAULT_READ_BYTES
        );
        // … and the small-record default is untouched at every reader count
        // (2 MiB × 12 readers is well inside the 64 MiB budget).
        for readers in [1_usize, 12] {
            assert_eq!(
                default_training_read_bytes_for_readers(40, &unknown, readers),
                DEFAULT_READ_BYTES
            );
        }
        // … while many readers share the one budget.
        for readers in [12_usize, 256] {
            let chunk =
                default_training_read_bytes_for_readers(PRODUCTION_RECORD_BYTES, &unknown, readers);
            assert!(
                chunk * readers <= AGGREGATE_READ_BUDGET_BYTES || chunk == PRODUCTION_RECORD_BYTES,
                "{readers} readers × {chunk} B must fit the mid-range budget"
            );
        }
    }

    #[test]
    fn aggregate_budget_tiers_are_reachable_and_ram_bounded() {
        // Both budget tiers must be consumable by the shipped defaults — the
        // ≥ 64 GiB tier is the 256 MiB figure the removed `max_read_bytes`
        // branch used to supply, so if nothing can reach it, it is dead too.
        let mid = HostResources::synthetic(10, Some(16 * GIB));
        let large = HostResources::synthetic(24, Some(64 * GIB));
        assert_eq!(
            aggregate_read_budget_bytes(&mid),
            AGGREGATE_READ_BUDGET_BYTES
        );
        assert_eq!(
            aggregate_read_budget_bytes(&large),
            LARGE_RAM_AGGREGATE_READ_BUDGET_BYTES
        );
        for host in [mid, large] {
            let readers = shipped_readers(&host);
            let budget = aggregate_read_budget_bytes(&host);
            let chunk =
                default_training_read_bytes_for_readers(PRODUCTION_RECORD_BYTES, &host, readers);
            let footprint = chunk * readers;
            assert!(
                footprint <= budget && footprint + readers > budget,
                "the shipped default must consume the {budget} B budget, got {footprint} B"
            );
        }
        // A sub-1 GiB host is bounded by its RAM share, not by the tier.
        let tiny = HostResources::synthetic(2, Some(GIB / 2));
        assert_eq!(
            aggregate_read_budget_bytes(&tiny),
            usize::try_from((GIB / 2) / AGGREGATE_READ_RAM_SHARE_DIVISOR).expect("fits")
        );
    }

    #[test]
    fn one_whole_record_survives_an_absurd_reader_count() {
        // The floor is one record: a buffer narrower than that can never
        // complete a read, whatever the budget says. 65536 readers leaves each
        // one 1 KiB of a 64 MiB budget — well under a 9848 B record.
        let small = HostResources::synthetic(4, Some(GIB));
        let chunk = default_training_read_bytes_for_readers(PRODUCTION_RECORD_BYTES, &small, 65536);
        assert_eq!(chunk, PRODUCTION_RECORD_BYTES);
    }

    #[test]
    fn env_override_and_record_alignment_are_unchanged_by_the_reader_bound() {
        // The override wins over the reader-count bound and is still clamped to
        // `[record_bytes, max_read_bytes]` and rounded to whole records.
        let rb = PRODUCTION_RECORD_BYTES;
        for readers in [1_usize, 12, 64] {
            let target = training_read_target_bytes_from_env_for_readers(rb, readers);
            assert_eq!(target % rb, 0, "whole records only");
            assert!(target >= rb, "at least one record wide");
            assert!(target <= max_read_bytes());
        }
    }
}
