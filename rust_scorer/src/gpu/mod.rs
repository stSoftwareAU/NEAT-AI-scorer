//! Optional GPU device detection for `rust_scorer` (Issue #80).
//!
//! This module wires `wgpu` adapter selection and an opt-in CLI flag /
//! environment variable into the scorer. Subsequent issues (#81 onward) will
//! consume the returned [`GpuContext`] to run real kernels — for now the
//! plumbing exists only so the binary can report which backend it would use
//! via the new `gpuBackend` JSON field, while the default CPU pipeline stays
//! byte-for-byte unchanged.
//!
//! ```text
//!   --gpu auto|on|off  ->  NEAT_SCORER_GPU=auto|on|off  ->  GpuMode
//!   GpuMode::Off                       -> GpuBackendLabel::CpuFallback
//!   GpuMode::Auto, no compatible GPU   -> GpuBackendLabel::CpuFallback
//!   GpuMode::Auto, GPU found           -> GpuBackendLabel::{Metal,Vulkan,Dx12,Gl}
//!   GpuMode::On, no compatible GPU     -> non-zero exit (caller decides)
//!   GpuMode::On, GPU found             -> GpuBackendLabel::{Metal,Vulkan,Dx12,Gl}
//! ```
//!
//! Issue #82 — multi-creature batched dispatch lives in the
//! [`forward_mse_batched`] submodule; the directory-mode scorer in
//! `multi_score::score_from_creature_dir` consumes it through
//! [`forward_mse_batched::BatchedRunner`] when `--gpu auto|on` resolves to a
//! native backend.
//!
//! ## Default mode (Issue #83)
//!
//! [`GpuMode::default()`] is [`GpuMode::Auto`]. End-to-end benchmarking
//! ([`docs/performance-baseline.md`](../../../docs/performance-baseline.md))
//! showed the GPU multi-creature batched kernel beats CPU+PGO by **≥ 30 %** at
//! `BENCH_SCORING_BYTES=200000000` for `score_from_creature_dir/creatures/50`
//! on Apple Silicon Metal, well above the 3 % acceptance threshold from
//! [`docs/gpu-scoring-design.md`](../../../docs/gpu-scoring-design.md). The
//! single-creature path stays on CPU — Issue #81 closed as a negative result.
//!
//! [`auto_should_use_gpu`] codifies that ship/skip decision as a runtime
//! function on a [`ScoringPath`] discriminant; `main.rs` consults it before
//! dispatching to the GPU runner. When `--gpu auto` finds no compatible
//! adapter, every path silently falls back to CPU — `auto` must never abort
//! scoring.

pub mod forward_mse_batched;

use std::str::FromStr;

/// Opt-in GPU selection mode parsed from `--gpu` (CLI) and the
/// `NEAT_SCORER_GPU` environment variable.
///
/// Resolution order: CLI flag wins over env var, env var wins over the
/// [`GpuMode::Off`] default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum GpuMode {
    /// Probe for a compatible GPU and silently fall back to CPU when none is
    /// available. **Default** since Issue #83: the multi-creature batched
    /// kernel from #82 beats CPU+PGO by ≥ 30 % at the issue-target corpus
    /// size on Apple Silicon Metal. The single-creature path keeps running
    /// on CPU — see [`auto_should_use_gpu`].
    #[default]
    Auto,
    /// Require a compatible GPU. The caller must treat a missing adapter as
    /// a hard failure (non-zero exit).
    On,
    /// Skip GPU detection entirely and run the CPU pipeline. Use this to
    /// reproduce pre-#83 behaviour or when the GPU kernel's parity tolerance
    /// is unsuitable.
    Off,
}

impl FromStr for GpuMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "on" => Ok(Self::On),
            "off" => Ok(Self::Off),
            other => Err(format!(
                "Invalid GPU mode '{other}': expected one of 'auto', 'on', 'off'"
            )),
        }
    }
}

/// Stable JSON label for the GPU backend that scoring actually used.
///
/// The serialised form (kebab-case) is what shows up in the new `gpuBackend`
/// field of `ScoreResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GpuBackendLabel {
    Metal,
    Vulkan,
    Dx12,
    Gl,
    /// No GPU was selected — either `--gpu off` (the default), or `--gpu auto`
    /// on a host with no compatible adapter.
    CpuFallback,
}

impl GpuBackendLabel {
    /// Stable serialised label as a `&'static str`. Matches the JSON form.
    #[allow(dead_code)] // used by tests; consumed by callers once GPU kernels land.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Metal => "metal",
            Self::Vulkan => "vulkan",
            Self::Dx12 => "dx12",
            Self::Gl => "gl",
            Self::CpuFallback => "cpu-fallback",
        }
    }

    /// Map a `wgpu::Backend` to a stable [`GpuBackendLabel`].
    ///
    /// `Noop` and `BrowserWebGpu` are treated as `CpuFallback` here — neither
    /// is a "real" native GPU backend the scorer can run kernels on, and
    /// returning `CpuFallback` keeps the JSON contract simple (only one label
    /// per "no GPU available" path).
    pub fn from_wgpu(b: wgpu::Backend) -> Self {
        match b {
            wgpu::Backend::Metal => Self::Metal,
            wgpu::Backend::Vulkan => Self::Vulkan,
            wgpu::Backend::Dx12 => Self::Dx12,
            wgpu::Backend::Gl => Self::Gl,
            // Noop / BrowserWebGpu — not a usable native GPU device.
            _ => Self::CpuFallback,
        }
    }
}

/// Errors that can occur while selecting a GPU device.
///
/// Returning `Ok(None)` means "no compatible adapter found" — the caller
/// decides whether that is a soft fall-back (`--gpu auto`) or a hard error
/// (`--gpu on`). `GpuInitError` is reserved for genuine failures *after* an
/// adapter was found (e.g. `request_device` rejected).
#[derive(Debug)]
pub enum GpuInitError {
    /// `wgpu::Adapter::request_device` failed.
    DeviceRequest(String),
}

impl std::fmt::Display for GpuInitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeviceRequest(e) => {
                write!(f, "Failed to request wgpu device: {e}")
            }
        }
    }
}

impl std::error::Error for GpuInitError {}

/// A successfully-initialised GPU context ready to drive kernels.
///
/// For Issue #80 nothing consumes `device`/`queue` yet — they exist so the
/// follow-up GPU kernel work in #81 can plug straight in without churning
/// this module's public API.
#[allow(dead_code)] // device/queue used by follow-up GPU kernel issues (#81+).
pub struct GpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub backend: GpuBackendLabel,
}

/// Which scoring entry point is about to run. Used by [`auto_should_use_gpu`]
/// to pick CPU or GPU under [`GpuMode::Auto`] (Issue #83).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoringPath {
    /// Single-creature scoring (`<creature.json> <data_dir>` or
    /// `--creature-stdin <data_dir>`). Issue #81 closed as a negative result:
    /// CPU+PGO beat the proposed single-creature GPU kernel at the
    /// issue-target corpus size, so this path stays on CPU under `Auto`.
    ///
    /// Constructed by tests and external library callers; the binary
    /// short-circuits the single-creature path before consulting the helper
    /// (no GPU kernel exists for it). The variant is part of the stable
    /// public API so future kernels can flip the decision in one place.
    #[allow(dead_code)]
    SingleCreature,
    /// Directory-of-creatures scoring (`<creatures_dir> <data_dir>`). Issue
    /// #82 showed the batched kernel beats CPU+PGO by ≥ 30 % at N=50 with
    /// `BENCH_SCORING_BYTES=200000000` on Apple Silicon Metal — so this path
    /// uses GPU under `Auto` whenever an adapter is available.
    CreatureDirectory,
}

/// Whether [`GpuMode::Auto`] should pick the GPU pipeline for the given
/// scoring path **and cost kind** (Issue #121).
///
/// This is the codified ship/skip decision from Issue #83 — call sites in
/// `main.rs` consult it instead of hard-coding "directory ⇒ GPU,
/// single ⇒ CPU" inline. Any future re-evaluation only has to update this
/// function (and the corresponding section in
/// [`docs/performance-baseline.md`](../../../docs/performance-baseline.md)).
///
/// Returns `true` only when:
/// 1. The bench evidence supports GPU at `BENCH_SCORING_BYTES=200000000`
///    being ≥ 3 % faster than CPU+PGO for the path, **and**
/// 2. The CPU↔GPU parity tolerance from #81 holds for the kernel, **and**
/// 3. The requested cost is one the GPU kernel actually implements
///    ([`crate::cost::CostKind::gpu_supported`] — currently MSE only).
///
/// `false` for every path means "no GPU paths shipped as default" — i.e. the
/// negative-result outcome from the Performance Task Workflow.
pub fn auto_should_use_gpu(path: ScoringPath, cost: crate::cost::CostKind) -> bool {
    // Issue #121: the GPU `forward_mse_batched` kernel only computes MSE.
    // Any other cost forces a silent CPU fallback under Auto.
    if !cost.gpu_supported() {
        return false;
    }
    match path {
        // #81 — negative result. CPU+PGO wins on the single-creature path.
        ScoringPath::SingleCreature => false,
        // #82 — N=50 / 200 MB corpus on Apple Silicon Metal:
        //   GPU 2.176 s vs CPU+PGO ≈ 2.96 s ⇒ ≈ 27 % faster (≥ 3 % bar met).
        ScoringPath::CreatureDirectory => true,
    }
}

/// Issue #205: one informational stderr note for the otherwise-silent
/// "non-MSE cost forced the CPU path under `--gpu auto`" fallback.
///
/// Under the default `--gpu auto`, selecting a non-MSE `--cost` makes
/// [`auto_should_use_gpu`] return `false`, so the directory path runs on
/// CPU. Unlike the explicit `--gpu on` hard-error and the GPU-runner
/// failure case, that fallback prints nothing — the only signal is the
/// `gpuBackend: cpu-fallback` JSON field, so users cannot tell their cost
/// choice (rather than a missing GPU) caused the CPU path.
///
/// Returns `Some(note)` — mirroring the existing `[gpu] auto fallback ...`
/// messages and naming the cost as the reason — only when **all** hold:
/// * `mode` is [`GpuMode::Auto`] (explicit `on`/`off` are unaffected),
/// * `path` is [`ScoringPath::CreatureDirectory`] (the only GPU-default
///   path), and
/// * `cost` is not GPU-supported ([`crate::cost::CostKind::gpu_supported`]).
///
/// Returns `None` otherwise (MSE / GPU-supported costs emit no extra output).
pub fn auto_cost_fallback_note(
    mode: GpuMode,
    path: ScoringPath,
    cost: crate::cost::CostKind,
) -> Option<String> {
    if !matches!(mode, GpuMode::Auto) || cost.gpu_supported() {
        return None;
    }
    match path {
        ScoringPath::CreatureDirectory => Some(format!(
            "[gpu] auto fallback to CPU directory mode: cost {} is not GPU-supported \
             (forward_mse_batched only handles MSE); rerun with --gpu off to skip GPU detection",
            cost.as_str()
        )),
        // The single-creature path is always CPU regardless of cost, so there
        // is no cost-driven fallback to explain.
        ScoringPath::SingleCreature => None,
    }
}

/// Resolve the final [`GpuMode`] from the CLI flag and the `NEAT_SCORER_GPU`
/// environment variable. CLI wins; otherwise the env var; otherwise default.
///
/// `cli` is the value already parsed by clap. `env` is whatever the caller
/// pulled from `std::env::var("NEAT_SCORER_GPU")` (or `None`). Returning a
/// `Result` lets the caller surface env-var typos as a clear error rather
/// than silently falling back.
pub fn resolve_mode(cli: Option<GpuMode>, env: Option<&str>) -> Result<GpuMode, String> {
    if let Some(m) = cli {
        return Ok(m);
    }
    if let Some(s) = env
        && !s.trim().is_empty()
    {
        return s.parse();
    }
    Ok(GpuMode::default())
}

/// Select a GPU adapter, preferring high-performance discrete GPUs.
///
/// Returns:
/// * `Ok(Some(ctx))` — a usable adapter was found and the device was
///   created. `ctx.backend` reports the actual native backend.
/// * `Ok(None)` — no compatible adapter is available (e.g. CPU-only host).
///   The caller should fall back to CPU for `--gpu auto` or error out for
///   `--gpu on`.
/// * `Err(GpuInitError)` — an adapter was found but `request_device`
///   failed. The caller should treat this as a hard error.
pub fn select_adapter() -> Result<Option<GpuContext>, GpuInitError> {
    // `new_without_display_handle_from_env` honours `WGPU_BACKEND` etc. and
    // does not require a window — the scorer is headless.
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    })) {
        Ok(a) => a,
        Err(_) => return Ok(None),
    };

    let backend = GpuBackendLabel::from_wgpu(adapter.get_info().backend);
    if backend == GpuBackendLabel::CpuFallback {
        // wgpu picked a non-native backend (Noop / BrowserWebGpu) — treat as
        // "no GPU" so the JSON label and behaviour line up with the
        // explicit-fallback path.
        return Ok(None);
    }

    // Issue #182 — request the adapter's full limits rather than wgpu's
    // conservative defaults (128 MiB `max_storage_buffer_binding_size`). The
    // large-creature `forward_mse_scratch` kernel sizes its activation scratch
    // against this limit; on Apple Silicon the adapter supports buffers far
    // larger than the default, so honouring it lets bigger creature sets run
    // with more grid-stride parallelism.
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("rust_scorer GPU device"),
        required_limits: adapter.limits(),
        ..wgpu::DeviceDescriptor::default()
    }))
    .map_err(|e| GpuInitError::DeviceRequest(e.to_string()))?;

    Ok(Some(GpuContext {
        device,
        queue,
        backend,
    }))
}

/// Resolve the GPU backend label for a given mode without leaking the
/// `GpuContext` (useful when callers only care about the JSON label, e.g.
/// the scorer entry points which do not yet run GPU kernels).
///
/// Returns:
/// * `Ok(label)` — the label to record in JSON.
/// * `Err(message)` — only when `mode == GpuMode::On` and no compatible
///   adapter was available, or `request_device` failed. The caller should
///   exit non-zero and surface the message to stderr.
#[allow(dead_code)] // used by benches/tests; main.rs now resolves the adapter directly so it
// can keep the resulting `GpuContext` for the GPU multi-creature path (Issue #82).
pub fn resolve_backend(mode: GpuMode) -> Result<GpuBackendLabel, String> {
    match mode {
        GpuMode::Off => Ok(GpuBackendLabel::CpuFallback),
        GpuMode::Auto => match select_adapter() {
            Ok(Some(ctx)) => Ok(ctx.backend),
            Ok(None) => Ok(GpuBackendLabel::CpuFallback),
            // Auto must never abort scoring — log nothing, just fall back.
            Err(_) => Ok(GpuBackendLabel::CpuFallback),
        },
        GpuMode::On => match select_adapter() {
            Ok(Some(ctx)) => Ok(ctx.backend),
            Ok(None) => Err(
                "No compatible GPU adapter found and --gpu on was requested (use --gpu auto to fall back to CPU, or --gpu off to skip GPU detection entirely)".to_string(),
            ),
            Err(e) => Err(e.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_mode_parses_lowercase() {
        assert_eq!(GpuMode::from_str("auto").unwrap(), GpuMode::Auto);
        assert_eq!(GpuMode::from_str("on").unwrap(), GpuMode::On);
        assert_eq!(GpuMode::from_str("off").unwrap(), GpuMode::Off);
    }

    #[test]
    fn gpu_mode_parses_with_whitespace_and_case() {
        assert_eq!(GpuMode::from_str("  Auto ").unwrap(), GpuMode::Auto);
        assert_eq!(GpuMode::from_str("ON").unwrap(), GpuMode::On);
        assert_eq!(GpuMode::from_str("OFF").unwrap(), GpuMode::Off);
    }

    #[test]
    fn gpu_mode_rejects_invalid() {
        let err = GpuMode::from_str("gpu").unwrap_err();
        assert!(
            err.contains("auto"),
            "error message should mention 'auto': {err}"
        );
        assert!(err.contains("on"));
        assert!(err.contains("off"));
        // Empty string is invalid (resolve_mode handles "no env var" via Option).
        assert!(GpuMode::from_str("").is_err());
    }

    #[test]
    fn gpu_mode_default_is_auto() {
        // Issue #83 flipped the default to Auto once the multi-creature
        // kernel from #82 cleared the 3 % CPU+PGO win threshold. Locking the
        // default in a test guards against accidental rollback.
        assert_eq!(GpuMode::default(), GpuMode::Auto);
    }

    #[test]
    fn auto_should_use_gpu_single_creature_stays_cpu() {
        // Issue #81 closed as a negative result — single-creature GPU lost
        // to CPU+PGO at the issue-target corpus size, so Auto must keep this
        // path on CPU.
        assert!(!auto_should_use_gpu(
            ScoringPath::SingleCreature,
            crate::cost::CostKind::Mse
        ));
    }

    #[test]
    fn auto_should_use_gpu_directory_uses_gpu() {
        // Issue #82 — multi-creature batched dispatch at N=50 / 200 MB beat
        // CPU+PGO by ≥ 30 % on Apple Silicon Metal, well above the 3 % bar.
        assert!(auto_should_use_gpu(
            ScoringPath::CreatureDirectory,
            crate::cost::CostKind::Mse
        ));
    }

    /// Issue #121: Auto must keep the directory path on CPU when the
    /// requested cost is not one the GPU kernel implements — today that
    /// means every cost except MSE forces CPU fallback.
    #[test]
    fn auto_should_use_gpu_directory_falls_back_to_cpu_for_non_mse_costs() {
        for cost in [
            crate::cost::CostKind::Mae,
            crate::cost::CostKind::Mape,
            crate::cost::CostKind::Msle,
            crate::cost::CostKind::Hinge,
            crate::cost::CostKind::CrossEntropy,
            crate::cost::CostKind::CategoricalError,
        ] {
            assert!(
                !auto_should_use_gpu(ScoringPath::CreatureDirectory, cost),
                "Auto must fall back to CPU for non-MSE cost {} until a kernel ships",
                cost.as_str()
            );
        }
    }

    /// Issue #205: a non-MSE cost under `--gpu auto` must surface one stderr
    /// note on the directory path, naming the cost as the reason for the CPU
    /// fallback.
    #[test]
    fn auto_cost_fallback_note_present_for_non_mse_directory() {
        for cost in [
            crate::cost::CostKind::Mae,
            crate::cost::CostKind::Mape,
            crate::cost::CostKind::Msle,
            crate::cost::CostKind::Hinge,
            crate::cost::CostKind::CrossEntropy,
            crate::cost::CostKind::CategoricalError,
        ] {
            let note = auto_cost_fallback_note(GpuMode::Auto, ScoringPath::CreatureDirectory, cost)
                .unwrap_or_else(|| panic!("expected a fallback note for cost {}", cost.as_str()));
            assert!(
                note.contains("[gpu] auto fallback"),
                "note should mirror the existing fallback messages: {note}"
            );
            assert!(
                note.contains(cost.as_str()),
                "note should name the cost {} as the reason: {note}",
                cost.as_str()
            );
        }
    }

    /// Issue #205: MSE (the GPU-supported cost) must not emit any note.
    #[test]
    fn auto_cost_fallback_note_absent_for_mse_directory() {
        assert_eq!(
            auto_cost_fallback_note(
                GpuMode::Auto,
                ScoringPath::CreatureDirectory,
                crate::cost::CostKind::Mse
            ),
            None
        );
    }

    /// Issue #205: explicit `--gpu on|off` are unaffected — no note even for a
    /// non-MSE cost (`on` hard-errors elsewhere; `off` never touches the GPU).
    #[test]
    fn auto_cost_fallback_note_absent_for_explicit_modes() {
        for mode in [GpuMode::On, GpuMode::Off] {
            assert_eq!(
                auto_cost_fallback_note(
                    mode,
                    ScoringPath::CreatureDirectory,
                    crate::cost::CostKind::Mae
                ),
                None,
                "explicit mode {mode:?} must not emit the auto fallback note"
            );
        }
    }

    /// Issue #205: the single-creature path is always CPU regardless of cost,
    /// so there is no cost-driven fallback to explain.
    #[test]
    fn auto_cost_fallback_note_absent_for_single_creature() {
        assert_eq!(
            auto_cost_fallback_note(
                GpuMode::Auto,
                ScoringPath::SingleCreature,
                crate::cost::CostKind::Mae
            ),
            None
        );
    }

    #[test]
    fn resolve_mode_cli_wins_over_env() {
        assert_eq!(
            resolve_mode(Some(GpuMode::On), Some("off")).unwrap(),
            GpuMode::On
        );
        assert_eq!(
            resolve_mode(Some(GpuMode::Off), Some("auto")).unwrap(),
            GpuMode::Off
        );
    }

    #[test]
    fn resolve_mode_falls_back_to_env_then_default() {
        assert_eq!(resolve_mode(None, Some("auto")).unwrap(), GpuMode::Auto);
        assert_eq!(resolve_mode(None, Some("on")).unwrap(), GpuMode::On);
        assert_eq!(resolve_mode(None, Some("off")).unwrap(), GpuMode::Off);
        // No env var → default mode (Auto since Issue #83).
        assert_eq!(resolve_mode(None, None).unwrap(), GpuMode::Auto);
        // Empty / whitespace env var is treated as "unset" so the default applies.
        assert_eq!(resolve_mode(None, Some("")).unwrap(), GpuMode::Auto);
        assert_eq!(resolve_mode(None, Some("   ")).unwrap(), GpuMode::Auto);
    }

    #[test]
    fn resolve_mode_propagates_invalid_env_value() {
        let err = resolve_mode(None, Some("yes")).unwrap_err();
        assert!(err.contains("yes") || err.contains("auto"));
    }

    #[test]
    fn backend_labels_are_stable_kebab_case() {
        assert_eq!(GpuBackendLabel::Metal.as_str(), "metal");
        assert_eq!(GpuBackendLabel::Vulkan.as_str(), "vulkan");
        assert_eq!(GpuBackendLabel::Dx12.as_str(), "dx12");
        assert_eq!(GpuBackendLabel::Gl.as_str(), "gl");
        assert_eq!(GpuBackendLabel::CpuFallback.as_str(), "cpu-fallback");
    }

    #[test]
    fn backend_labels_serialise_as_kebab_case_strings() {
        // The JSON form is what `gpuBackend` ends up as in scorer output, so
        // pin it explicitly: existing downstream callers parse strings.
        let json = serde_json::to_string(&GpuBackendLabel::CpuFallback).unwrap();
        assert_eq!(json, "\"cpu-fallback\"");
        let json = serde_json::to_string(&GpuBackendLabel::Dx12).unwrap();
        assert_eq!(json, "\"dx12\"");
        let json = serde_json::to_string(&GpuBackendLabel::Metal).unwrap();
        assert_eq!(json, "\"metal\"");
    }

    #[test]
    fn from_wgpu_maps_native_backends() {
        assert_eq!(
            GpuBackendLabel::from_wgpu(wgpu::Backend::Metal),
            GpuBackendLabel::Metal
        );
        assert_eq!(
            GpuBackendLabel::from_wgpu(wgpu::Backend::Vulkan),
            GpuBackendLabel::Vulkan
        );
        assert_eq!(
            GpuBackendLabel::from_wgpu(wgpu::Backend::Dx12),
            GpuBackendLabel::Dx12
        );
        assert_eq!(
            GpuBackendLabel::from_wgpu(wgpu::Backend::Gl),
            GpuBackendLabel::Gl
        );
    }

    #[test]
    fn from_wgpu_maps_non_native_to_cpu_fallback() {
        assert_eq!(
            GpuBackendLabel::from_wgpu(wgpu::Backend::Noop),
            GpuBackendLabel::CpuFallback
        );
        assert_eq!(
            GpuBackendLabel::from_wgpu(wgpu::Backend::BrowserWebGpu),
            GpuBackendLabel::CpuFallback
        );
    }

    /// `--gpu off` MUST NOT touch `wgpu` at all and MUST always report
    /// `CpuFallback`. This is the primary acceptance criterion for #80
    /// ("byte-for-byte identical except for the new gpuBackend field").
    #[test]
    fn resolve_backend_off_returns_cpu_fallback() {
        assert_eq!(
            resolve_backend(GpuMode::Off).unwrap(),
            GpuBackendLabel::CpuFallback
        );
    }

    /// `--gpu auto` must never fail or panic, even on a CPU-only host. The
    /// returned label may be any native backend or `CpuFallback` depending
    /// on what `wgpu` finds, so we just assert the call succeeds.
    #[test]
    fn resolve_backend_auto_never_panics() {
        // No assertion on the variant — CI runners may or may not have a GPU.
        let _ = resolve_backend(GpuMode::Auto)
            .expect("--gpu auto must succeed even with no GPU available");
    }
}
