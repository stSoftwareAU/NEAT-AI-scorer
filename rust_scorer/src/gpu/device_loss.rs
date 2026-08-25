//! Device loss is an environmental event, not a scoring failure (Issue #583).
//!
//! `wgpu` reports a lost device **fatally**: every entry point that cannot map
//! the underlying error onto its own typed error calls `handle_error_fatal`,
//! which `panic!`s. `Device::poll` is the one the fleet hits — on a headless
//! Linux host whose Vulkan/Mesa loader drops the device mid-run it panics with
//!
//! ```text
//! Error in Device::poll: Validation Error
//!
//! Caused by:
//!   Parent device is lost
//! ```
//!
//! Unwinding out of the scorer exits **101**, which the NEAT-AI batch bridge
//! turns into a `ScorerStrictError` and an entire evolve stage dies. A `Result`
//! cannot catch it, because the panic happens *inside* `wgpu` before any
//! `Result` is returned — so the run boundary catches the unwind instead.
//!
//! ```text
//!   run_with_device_loss_fallback(mode, gpu, cpu)
//!     gpu() returns Ok            -> Ok(value)
//!     gpu() returns Err  (#273)   -> Auto: log once + cpu()   On: Err(diagnostic)
//!     gpu() panics (device lost)  -> Auto: log once + cpu()   On: Err(diagnostic)
//! ```
//!
//! Under `--gpu auto` that lands device loss exactly where an absent adapter
//! already lands: the CPU pipeline, valid JSON, exit 0. Under `--gpu on` the
//! run still fails — the user demanded a GPU — but with a diagnostic and
//! exit 1 rather than a panic and exit 101.

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::GpuMode;

/// Lower-cased fragments `wgpu` (and the backing Vulkan/Metal drivers) use when
/// a call failed because the device is gone.
///
/// Matching on the message is deliberate: the fatal path hands the caller an
/// unwind payload, not a typed error, so the text is the only signal that
/// survives.
const DEVICE_LOST_MARKERS: &[&str] = &[
    "device is lost",
    "device lost",
    "devicelost",
    "lost the device",
    "device was lost",
];

/// A GPU run that ended in an unwind rather than a returned error (Issue #583).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuRunFailure {
    /// The `wgpu` device was lost mid-run. Environmental: the same creature
    /// set scores fine on the CPU pipeline.
    DeviceLost(String),
    /// The GPU pipeline aborted for some other reason. Still recoverable under
    /// `--gpu auto` — `auto` must never abort scoring — but it is not an
    /// expected environmental event, so it is reported as its own variant.
    Panicked(String),
}

impl GpuRunFailure {
    /// The unwind payload text, whitespace-flattened to a single line.
    pub fn detail(&self) -> &str {
        match self {
            Self::DeviceLost(d) | Self::Panicked(d) => d,
        }
    }

    /// Was this a lost device (as opposed to any other abort)?
    pub fn is_device_lost(&self) -> bool {
        matches!(self, Self::DeviceLost(_))
    }
}

impl std::fmt::Display for GpuRunFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeviceLost(detail) => write!(
                f,
                "the GPU device was lost mid-run ({detail}) — an environmental fault, not a scoring failure"
            ),
            Self::Panicked(detail) => {
                write!(f, "the GPU pipeline aborted mid-run ({detail})")
            }
        }
    }
}

impl std::error::Error for GpuRunFailure {}

/// Does this message report a lost `wgpu` device?
///
/// Case-insensitive substring match against `DEVICE_LOST_MARKERS`, so it
/// classifies both the `wgpu` wording (`Parent device is lost`) and the
/// `DeviceLost` variant name a driver-level error may carry.
pub fn message_reports_device_loss(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    DEVICE_LOST_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}

/// Collapse every run of whitespace into a single space and trim the ends.
///
/// `wgpu`'s fatal message is multi-line (`Validation Error\n\nCaused by:\n
/// Parent device is lost`); the operator-facing fallback note must stay one
/// grep-able line.
pub(crate) fn flatten_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Recover the message from an unwind payload (`String` or `&'static str`).
pub(crate) fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Nesting depth of [`catch_gpu_panic`] regions currently running.
static GUARD_DEPTH: AtomicUsize = AtomicUsize::new(0);

/// Install (once) a panic hook that suppresses the default dump **only** for a
/// device-loss panic raised inside a guarded region.
///
/// Such a panic is not a crash: the payload is returned to the caller, which
/// logs one clear fallback line. Printing `thread 'main' panicked at …` as well
/// makes an orderly CPU fallback read like the exit-101 abort this issue
/// removes. Every other panic — inside a guarded region or not — keeps the
/// default hook, so a genuine bug is never silenced.
fn install_device_loss_hook() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let guarded_device_loss = GUARD_DEPTH.load(Ordering::SeqCst) > 0
                && info
                    .payload_as_str()
                    .is_some_and(message_reports_device_loss);
            if !guarded_device_loss {
                previous(info);
            }
        }));
    });
}

/// RAII marker: a guarded GPU region is running on some thread.
struct GuardDepth;

impl GuardDepth {
    fn enter() -> Self {
        install_device_loss_hook();
        GUARD_DEPTH.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for GuardDepth {
    fn drop(&mut self) {
        GUARD_DEPTH.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Run `f`, converting an unwind into a typed [`GpuRunFailure`].
///
/// This is the only defence against `wgpu`'s fatal error path: `Device::poll`,
/// `Queue::submit`, buffer creation and mapping all `panic!` when the device
/// has gone away, so no `Result` the GPU runner returns can report it.
pub fn catch_gpu_panic<T>(f: impl FnOnce() -> T) -> Result<T, GpuRunFailure> {
    let _depth = GuardDepth::enter();
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => Ok(value),
        Err(payload) => {
            let detail = flatten_whitespace(&panic_payload_message(payload.as_ref()));
            if message_reports_device_loss(&detail) {
                Err(GpuRunFailure::DeviceLost(detail))
            } else {
                Err(GpuRunFailure::Panicked(detail))
            }
        }
    }
}

/// Prefix shared by every `[gpu] auto fallback …` note (Issue #205 mirror).
const FALLBACK_NOTE_PREFIX: &str = "[gpu] auto fallback to CPU directory mode";

/// Build the single operator-facing line explaining why the run left the GPU.
///
/// Split out from [`run_with_device_loss_fallback`] so the wording is testable
/// without a GPU.
pub(crate) fn fallback_note(reason: &str) -> String {
    format!("{FALLBACK_NOTE_PREFIX}: {reason}; scoring continues on the CPU pipeline")
}

/// Run GPU directory scoring under the device-loss guard, degrading to `run_cpu`
/// when the GPU cannot finish the run (Issue #583).
///
/// * `Ok` from `run_gpu` — returned untouched.
/// * `Err` from `run_gpu` — the recoverable readback failure Issue #273 already
///   surfaced (`map_async` failed, worker hung up).
/// * an unwind out of `run_gpu` — device loss or any other GPU abort.
///
/// The last two are the same decision: under [`GpuMode::On`] the user demanded
/// a GPU, so the reason is returned as an error (the caller exits non-zero with
/// a diagnostic — never a panic). Under [`GpuMode::Auto`] (and defensively
/// under [`GpuMode::Off`], which never reaches here) the reason is logged
/// **once** to stderr and `run_cpu` produces the result, so the batch still
/// gets valid JSON and exit 0.
pub fn run_with_device_loss_fallback<T>(
    mode: GpuMode,
    run_gpu: impl FnOnce() -> Result<T, String>,
    run_cpu: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let reason = match catch_gpu_panic(run_gpu) {
        Ok(Ok(value)) => return Ok(value),
        Ok(Err(recoverable)) => recoverable,
        Err(failure) => failure.to_string(),
    };

    if matches!(mode, GpuMode::On) {
        // `--gpu on` is a hard requirement: fail loud, with the reason, and
        // leave the exit code to the caller.
        return Err(reason);
    }

    eprintln!("{}", fallback_note(&reason));
    run_cpu()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact payload `wgpu` 29.0.4 panics with when the device is lost
    /// mid-run (from the fleet stage-failure dump quoted in Issue #583).
    const WGPU_DEVICE_LOST_PANIC: &str =
        "Error in Device::poll: Validation Error\n\nCaused by:\n  Parent device is lost\n";

    #[test]
    fn classifies_the_wgpu_device_lost_message() {
        assert!(message_reports_device_loss(WGPU_DEVICE_LOST_PANIC));
        assert!(message_reports_device_loss("Parent device is lost"));
        assert!(message_reports_device_loss("DeviceLost"));
        assert!(message_reports_device_loss("The device was lost"));
    }

    #[test]
    fn does_not_classify_unrelated_failures_as_device_loss() {
        assert!(!message_reports_device_loss(
            "Error in Device::poll: Validation Error: buffer is already mapped"
        ));
        assert!(!message_reports_device_loss("n_records exceeds u32::MAX"));
        assert!(!message_reports_device_loss("out of memory"));
    }

    #[test]
    fn catches_a_device_lost_panic_as_a_typed_failure() {
        let failure = catch_gpu_panic(|| panic!("{WGPU_DEVICE_LOST_PANIC}"))
            .expect_err("a device-lost panic must be caught, not propagated");
        assert!(failure.is_device_lost(), "got {failure:?}");
        assert_eq!(
            failure.detail(),
            "Error in Device::poll: Validation Error Caused by: Parent device is lost",
            "the multi-line wgpu payload must flatten to one log line",
        );
    }

    #[test]
    fn catches_an_unrelated_panic_as_a_plain_abort() {
        let failure = catch_gpu_panic(|| panic!("n_records exceeds u32::MAX"))
            .expect_err("any GPU panic must be caught");
        assert_eq!(
            failure,
            GpuRunFailure::Panicked("n_records exceeds u32::MAX".to_string())
        );
    }

    #[test]
    fn passes_a_successful_gpu_run_through_untouched() {
        assert_eq!(catch_gpu_panic(|| 41 + 1), Ok(42));
    }

    #[test]
    fn device_loss_display_names_the_environmental_cause() {
        let text = GpuRunFailure::DeviceLost("Parent device is lost".to_string()).to_string();
        assert!(text.contains("GPU device was lost mid-run"), "got: {text}");
        assert!(text.contains("environmental fault"), "got: {text}");
    }

    #[test]
    fn auto_falls_back_to_cpu_when_the_device_is_lost() {
        let result: Result<&str, String> = run_with_device_loss_fallback(
            GpuMode::Auto,
            || panic!("{WGPU_DEVICE_LOST_PANIC}"),
            || Ok("cpu-scored"),
        );
        assert_eq!(result, Ok("cpu-scored"));
    }

    #[test]
    fn auto_falls_back_when_the_gpu_returns_a_recoverable_error() {
        // Issue #273's `map_async` failure path must keep behaving as before.
        let result: Result<&str, String> = run_with_device_loss_fallback(
            GpuMode::Auto,
            || Err("partials map_async failed: DeviceLost".to_string()),
            || Ok("cpu-scored"),
        );
        assert_eq!(result, Ok("cpu-scored"));
    }

    #[test]
    fn on_reports_device_loss_as_an_error_instead_of_panicking() {
        let result: Result<&str, String> = run_with_device_loss_fallback(
            GpuMode::On,
            || panic!("{WGPU_DEVICE_LOST_PANIC}"),
            || panic!("--gpu on must not run the CPU fallback"),
        );
        let err = result.expect_err("--gpu on must surface device loss as an error");
        assert!(err.contains("GPU device was lost mid-run"), "got: {err}");
    }

    #[test]
    fn a_successful_gpu_run_never_touches_the_cpu_fallback() {
        let result: Result<&str, String> = run_with_device_loss_fallback(
            GpuMode::Auto,
            || Ok("gpu-scored"),
            || panic!("CPU fallback must not run when the GPU succeeded"),
        );
        assert_eq!(result, Ok("gpu-scored"));
    }

    #[test]
    fn fallback_note_is_one_line_naming_the_cpu_pipeline() {
        let note =
            fallback_note(&GpuRunFailure::DeviceLost("Parent device is lost".into()).to_string());
        assert!(!note.contains('\n'), "the note must stay one line: {note}");
        assert!(note.starts_with("[gpu] auto fallback to CPU directory mode:"));
        assert!(note.ends_with("scoring continues on the CPU pipeline"));
    }

    #[test]
    fn flattens_multi_line_payloads() {
        assert_eq!(flatten_whitespace("a\n\n  b \tc\n"), "a b c");
    }

    #[test]
    fn reads_both_str_and_string_panic_payloads() {
        let owned: Box<dyn Any + Send> = Box::new("borrowed payload");
        assert_eq!(panic_payload_message(owned.as_ref()), "borrowed payload");
        let owned: Box<dyn Any + Send> = Box::new("owned payload".to_string());
        assert_eq!(panic_payload_message(owned.as_ref()), "owned payload");
        let owned: Box<dyn Any + Send> = Box::new(7_u8);
        assert_eq!(
            panic_payload_message(owned.as_ref()),
            "non-string panic payload"
        );
    }
}
