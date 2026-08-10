//! `--host-report`: what this host detected and which knob values it resolved.
//!
//! Issue #545 (sub-issue of #544). Every host-tuned knob is chosen from a
//! probed [`HostResources`](crate::host_resources::HostResources) snapshot, so
//! retuning one across a 20+ machine fleet first needs a way to *see* what a
//! given host actually detected and chose. This module renders that as one
//! JSON object, each knob tagged with whether the value came from the built-in
//! default or from an environment override.
//!
//! It is **measurement only** — resolving the report changes no default and
//! never creates a `wgpu` adapter, so it runs unchanged on a GPU-less host and
//! under `--gpu off`.
//!
//! Keys are snake_case and mirror the Rust functions that produced them
//! (`default_worker_count`, `max_read_bytes`, …) so a pasted report line maps
//! 1:1 onto the code a retune sub-issue has to change. This is deliberately
//! *not* the camelCase [`ScoreResult`](crate::scoring::ScoreResult) contract —
//! the report is a diagnostic, not part of the TypeScript bridge's scoring
//! payload.

use serde::Serialize;

use crate::env_tuning::parse_tuning_var;
use crate::gpu::forward_mse_batched::scratch_budget_bytes_from_env;
use crate::host_resources;
use crate::read_tuning::training_read_target_bytes_from_env;
use crate::stream_score::activation_worker_count_for_scorer;

/// Record width the read-chunk knob is resolved for when `--record-bytes` is
/// omitted: the production corpus shape (2461 inputs + 1 output, `f32`).
///
/// The read default is record-size adaptive
/// (`read_tuning::default_training_read_bytes_for`), so a report with
/// no width would describe a knob nobody runs.
pub const DEFAULT_REPORT_RECORD_BYTES: usize = 9848;

/// Report schema tag, so a consumer can tell an old paste from a new one.
///
/// `/2` adds `performance_cpus` (Issue #546) alongside `logical_cpus`.
const SCHEMA: &str = "neat-scorer-host-report/2";

/// Where a resolved knob value came from.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KnobSource {
    /// The built-in, host-derived default (no override, or one that was
    /// rejected as malformed and therefore never applied).
    Default,
    /// An environment override that was parsed and honoured.
    Env,
}

/// One resolved knob: the value the scorer will actually use, plus its origin.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Knob {
    /// Resolved value, after every clamp the shipped resolver applies.
    pub value: u64,
    /// Built-in default or honoured environment override.
    pub source: KnobSource,
    /// Environment variable that can override this knob, or `None` for a
    /// host-derived ceiling that takes no override.
    pub env_var: Option<&'static str>,
}

/// Every knob a #544 retune sub-issue may move.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Knobs {
    /// Activation / file-reader workers the scorer defaults to
    /// ([`host_resources::default_worker_count`]), after
    /// `NEAT_SCORER_ACTIVATION_THREADS`. `NEAT_SCORER_FILE_THREADS` shares this
    /// default, additionally capped at the corpus file count.
    pub default_worker_count: Knob,
    /// Host ceiling both worker knobs clamp to.
    pub max_worker_count: Knob,
    /// Host ceiling `NEAT_SCORER_READ_BYTES` clamps to.
    pub max_read_bytes: Knob,
    /// Read-chunk target for `record_bytes`-wide records, after
    /// `NEAT_SCORER_READ_BYTES` and the round-down to a whole record multiple.
    pub default_training_read_bytes: Knob,
    /// `forward_mse_scratch` SSBO budget, after `NEAT_SCORER_GPU_SCRATCH_BYTES`.
    /// Resolved from host RAM alone — no adapter is created.
    pub gpu_scratch_bytes: Knob,
}

/// One host's detected resources and resolved knobs.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostReport {
    /// Schema tag (`neat-scorer-host-report/2`).
    pub schema: &'static str,
    /// Logical CPUs the host probe found.
    pub logical_cpus: usize,
    /// Performance ("P") cores the host probe found (Issue #546). Equal to
    /// `logical_cpus` on a homogeneous host, or wherever the platform exposes
    /// no P/E split — the probe never reports fewer than it can prove.
    pub performance_cpus: usize,
    /// Physical RAM in bytes, or `null` where the platform probe is
    /// unavailable (the knobs then take their unknown-RAM defaults).
    pub physical_ram_bytes: Option<u64>,
    /// Record width `default_training_read_bytes` was resolved for.
    pub record_bytes: usize,
    /// The resolved knobs.
    pub knobs: Knobs,
}

impl HostReport {
    /// Resolve the report for this process: probe the host, read the
    /// environment, and run the same resolvers the scoring paths use.
    #[must_use]
    pub fn resolve(record_bytes: usize) -> Self {
        let host = host_resources::host();
        Self {
            schema: SCHEMA,
            logical_cpus: host.cpus,
            performance_cpus: host.performance_cpus,
            physical_ram_bytes: host.physical_ram_bytes,
            record_bytes,
            knobs: Knobs {
                default_worker_count: Knob {
                    value: activation_worker_count_for_scorer() as u64,
                    source: env_source_usize("NEAT_SCORER_ACTIVATION_THREADS"),
                    env_var: Some("NEAT_SCORER_ACTIVATION_THREADS"),
                },
                max_worker_count: host_ceiling(host_resources::max_worker_count(&host) as u64),
                max_read_bytes: host_ceiling(host_resources::max_read_bytes(&host) as u64),
                default_training_read_bytes: Knob {
                    value: training_read_target_bytes_from_env(record_bytes) as u64,
                    source: env_source_usize("NEAT_SCORER_READ_BYTES"),
                    env_var: Some("NEAT_SCORER_READ_BYTES"),
                },
                gpu_scratch_bytes: Knob {
                    value: scratch_budget_bytes_from_env(),
                    source: env_source_positive_u64("NEAT_SCORER_GPU_SCRATCH_BYTES"),
                    env_var: Some("NEAT_SCORER_GPU_SCRATCH_BYTES"),
                },
            },
        }
    }

    /// Render the report as one pretty JSON object.
    ///
    /// # Errors
    ///
    /// Returns the `serde_json` failure text if the report cannot be
    /// serialised, so the caller can fail loud rather than print nothing.
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialise host report to JSON: {e}"))
    }
}

/// A knob with no override: a host-derived ceiling.
const fn host_ceiling(value: u64) -> Knob {
    Knob {
        value,
        source: KnobSource::Default,
        env_var: None,
    }
}

/// Would `name`'s current value be honoured by a `usize` knob resolver?
fn env_source_usize(name: &str) -> KnobSource {
    source_from_raw(name, std::env::var(name).ok().as_deref(), 0usize, |s| {
        s.parse::<usize>().ok()
    })
}

/// Would `name`'s current value be honoured by the GPU scratch resolver?
///
/// That resolver additionally rejects `0` as semantically invalid, so the same
/// predicate is applied here.
fn env_source_positive_u64(name: &str) -> KnobSource {
    source_from_raw(name, std::env::var(name).ok().as_deref(), 0u64, |s| {
        s.parse::<u64>().ok().filter(|&b| b > 0)
    })
}

/// Classify a raw override value using the **shipped** acceptance predicate.
///
/// Unset, blank, and malformed values all fall back to the built-in default in
/// [`parse_tuning_var`], so only a value that parser accepts is reported as
/// `env`. Routing through the same helper keeps the reported source from
/// drifting away from what the resolver actually did.
fn source_from_raw<T, F>(name: &str, raw: Option<&str>, probe_default: T, parse: F) -> KnobSource
where
    T: std::fmt::Display,
    F: FnOnce(&str) -> Option<T>,
{
    let Some(raw) = raw else {
        return KnobSource::Default;
    };
    if raw.trim().is_empty() {
        return KnobSource::Default;
    }
    let (_value, warning) = parse_tuning_var(name, Some(raw), probe_default, parse);
    if warning.is_some() {
        KnobSource::Default
    } else {
        KnobSource::Env
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_usize(s: &str) -> Option<usize> {
        s.parse::<usize>().ok()
    }

    #[test]
    fn unset_and_blank_overrides_report_the_default_source() {
        assert_eq!(
            source_from_raw("NEAT_SCORER_READ_BYTES", None, 0usize, parse_usize),
            KnobSource::Default
        );
        for raw in ["", "   ", "\t"] {
            assert_eq!(
                source_from_raw("NEAT_SCORER_READ_BYTES", Some(raw), 0usize, parse_usize),
                KnobSource::Default,
                "blank {raw:?} is not an override"
            );
        }
    }

    #[test]
    fn honoured_override_reports_the_env_source() {
        assert_eq!(
            source_from_raw(
                "NEAT_SCORER_READ_BYTES",
                Some("4194304"),
                0usize,
                parse_usize
            ),
            KnobSource::Env
        );
        // Surrounding whitespace is trimmed by the shared parser, so the value
        // is still honoured — and must still read as an override.
        assert_eq!(
            source_from_raw(
                "NEAT_SCORER_READ_BYTES",
                Some(" 4194304 "),
                0usize,
                parse_usize
            ),
            KnobSource::Env
        );
    }

    #[test]
    fn malformed_override_reports_the_default_source() {
        // The shipped resolver warns and falls back on these, so the report
        // must not claim an override that never applied.
        for raw in ["2MB", "-1", "lots"] {
            assert_eq!(
                source_from_raw("NEAT_SCORER_READ_BYTES", Some(raw), 0usize, parse_usize),
                KnobSource::Default,
                "malformed {raw:?} must not read as an override"
            );
        }
    }

    #[test]
    fn zero_gpu_scratch_is_not_an_override() {
        // `scratch_budget_bytes_from_env` rejects zero as semantically invalid;
        // the report applies the same predicate.
        let parse = |s: &str| s.parse::<u64>().ok().filter(|&b| b > 0);
        assert_eq!(
            source_from_raw("NEAT_SCORER_GPU_SCRATCH_BYTES", Some("0"), 0u64, parse),
            KnobSource::Default
        );
        assert_eq!(
            source_from_raw(
                "NEAT_SCORER_GPU_SCRATCH_BYTES",
                Some("134217728"),
                0u64,
                parse
            ),
            KnobSource::Env
        );
    }

    #[test]
    fn report_serialises_every_knob_with_a_source() {
        let report = HostReport::resolve(DEFAULT_REPORT_RECORD_BYTES);
        let json = report.to_json().expect("report must serialise");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(value["schema"], SCHEMA);
        assert_eq!(value["record_bytes"], DEFAULT_REPORT_RECORD_BYTES as u64);
        let logical = value["logical_cpus"].as_u64().expect("logical_cpus");
        assert!(logical >= 1);
        // Issue #546: the P-core count travels with the report so a pasted
        // worker default can be checked against the split it was chosen from.
        assert!(
            value["performance_cpus"]
                .as_u64()
                .is_some_and(|n| n >= 1 && n <= logical),
            "performance_cpus must be in 1..=logical_cpus, got {value}"
        );
        for key in [
            "default_worker_count",
            "max_worker_count",
            "max_read_bytes",
            "default_training_read_bytes",
            "gpu_scratch_bytes",
        ] {
            let knob = &value["knobs"][key];
            assert!(
                knob["value"].as_u64().is_some_and(|v| v > 0),
                "{key} must carry a positive value, got {knob}"
            );
            let source = knob["source"].as_str().unwrap_or_default();
            assert!(
                source == "default" || source == "env",
                "{key} source must be default|env, got {knob}"
            );
        }
        // Ceilings take no override, so they serialise a null `env_var`.
        assert!(value["knobs"]["max_worker_count"]["env_var"].is_null());
        assert_eq!(
            value["knobs"]["max_read_bytes"]["source"], "default",
            "a host ceiling is never an env override"
        );
    }

    #[test]
    fn read_default_tracks_the_requested_record_width() {
        // Record-size adaptive: a production-width record takes a bigger chunk
        // than a 40-byte one on every host whose RAM probe succeeds.
        let small = HostReport::resolve(40);
        let large = HostReport::resolve(DEFAULT_REPORT_RECORD_BYTES);
        assert_eq!(small.record_bytes, 40);
        assert_eq!(large.record_bytes, DEFAULT_REPORT_RECORD_BYTES);
        assert!(
            large.knobs.default_training_read_bytes.value
                >= small.knobs.default_training_read_bytes.value
        );
    }
}
