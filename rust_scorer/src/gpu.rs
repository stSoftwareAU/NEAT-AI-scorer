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
//! The default mode is [`GpuMode::Off`] until the kernel work in #81 lands;
//! existing callers that do not set `--gpu` keep the current CPU behaviour.

use std::str::FromStr;

/// Opt-in GPU selection mode parsed from `--gpu` (CLI) and the
/// `NEAT_SCORER_GPU` environment variable.
///
/// Resolution order: CLI flag wins over env var, env var wins over the
/// [`GpuMode::Off`] default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum GpuMode {
    /// Probe for a compatible GPU and silently fall back to CPU when none is
    /// available. Suitable as the default once GPU kernels are wired in.
    Auto,
    /// Require a compatible GPU. The caller must treat a missing adapter as
    /// a hard failure (non-zero exit).
    On,
    /// Skip GPU detection entirely and run the CPU pipeline. **Default** for
    /// this issue — kept until kernel work in #81 makes `Auto` safe.
    #[default]
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

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("rust_scorer GPU device"),
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
    fn gpu_mode_default_is_off() {
        // Default must stay Off until #81 makes Auto safe.
        assert_eq!(GpuMode::default(), GpuMode::Off);
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
        assert_eq!(resolve_mode(None, None).unwrap(), GpuMode::Off);
        // Empty env var is treated as "unset" so the default applies.
        assert_eq!(resolve_mode(None, Some("")).unwrap(), GpuMode::Off);
        assert_eq!(resolve_mode(None, Some("   ")).unwrap(), GpuMode::Off);
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
