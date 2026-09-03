//! External termination is an environmental event, not a scoring failure
//! (Issue #591).
//!
//! A GRQ-24 sampler run hit its 3-hour per-task cap mid-batch. `run_core`
//! signalled the process group, `rust_scorer` died on the **default**
//! disposition of `SIGUSR1`, and everything the NEAT-AI batch bridge could see
//! was an exit code and an unrelated GPU note:
//!
//! ```text
//! [scorer-strict] no-named-creature reason=EXEC_FAILURE exitCode=158
//! ScorerStrictError: Rust scorer batch call failed (exit 158) for 20 creature(s)
//! --- rust_scorer stderr ---
//! [gpu] auto fallback to CPU directory mode: …
//! ```
//!
//! Every other abnormal end of a scoring run already names itself — a lost GPU
//! device (Issue #583), a serialisation failure (Issue #201), a racing-protocol
//! fault (Issue #308). Being killed from outside was the one silent exit, so
//! the operator had nothing to separate "the cap killed it" from "the scorer
//! is broken".
//!
//! [`install`] closes that gap. Each catchable termination signal gets a
//! handler that writes **one** grep-able line to stderr and exits `128 + signo`
//! — the same status the caller already observed, now with a reason attached:
//!
//! ```text
//! [signal] rust_scorer terminated by SIGUSR1 (signal 30) — external termination, not a scoring failure; no result JSON was produced; exiting 158
//! ```
//!
//! The handler runs in signal context, so it must be async-signal-safe: it
//! allocates nothing, locks nothing and calls only `write(2)` and `_exit(2)`.
//! The line is rendered into a stack buffer by [`render_diagnostic`], and
//! `_exit` deliberately skips the stdout flush — a half-written result map is
//! worse than none, and a killed sweep has no result to report.

#[cfg(unix)]
use std::ffi::c_int;

/// The catchable signals that end a scoring run, with the name each is
/// reported under.
///
/// `SIGKILL` and `SIGSTOP` are absent because they cannot be caught: a
/// `kill -9` still dies silently, and no handler can change that.
#[cfg(unix)]
pub const HANDLED_SIGNALS: &[(c_int, &str)] = &[
    (libc::SIGHUP, "SIGHUP"),
    (libc::SIGINT, "SIGINT"),
    (libc::SIGQUIT, "SIGQUIT"),
    (libc::SIGTERM, "SIGTERM"),
    (libc::SIGUSR1, "SIGUSR1"),
    (libc::SIGUSR2, "SIGUSR2"),
];

/// Bytes reserved for the diagnostic line on the handler's stack.
///
/// Sized with headroom over the longest line any handled signal produces; the
/// `every_handled_signal_fits_the_buffer` test pins that it is enough.
pub const DIAGNOSTIC_BUF_BYTES: usize = 256;

/// The name reported for `signo`, or `None` when it is not one this module
/// handles.
#[cfg(unix)]
#[must_use]
pub fn signal_name(signo: c_int) -> Option<&'static str> {
    HANDLED_SIGNALS
        .iter()
        .find(|(candidate, _)| *candidate == signo)
        .map(|(_, name)| *name)
}

/// The conventional shell exit status for a death by `signo`: `128 + signo`.
#[must_use]
pub const fn exit_code(signo: i32) -> i32 {
    128 + signo
}

/// Render the operator-facing diagnostic for `signo` into `buf`, returning how
/// many bytes were written.
///
/// Allocation-free so the signal handler can call it. Writing stops at the end
/// of `buf` rather than panicking — a handler must never unwind — so callers
/// size `buf` with [`DIAGNOSTIC_BUF_BYTES`].
#[must_use]
pub fn render_diagnostic(buf: &mut [u8], name: &str, signo: i32) -> usize {
    let mut written = 0_usize;
    let mut push = |bytes: &[u8]| {
        let room = buf.len().saturating_sub(written);
        let take = room.min(bytes.len());
        buf[written..written + take].copy_from_slice(&bytes[..take]);
        written += take;
    };
    let mut digits = [0_u8; 12];

    push(b"[signal] rust_scorer terminated by ");
    push(name.as_bytes());
    push(b" (signal ");
    push(render_i32(&mut digits, signo));
    push(
        ") — external termination, not a scoring failure; no result JSON was produced; exiting "
            .as_bytes(),
    );
    push(render_i32(&mut digits, exit_code(signo)));
    push(b"\n");
    written
}

/// Format `value` into `digits` without allocating, returning the filled slice.
fn render_i32(digits: &mut [u8; 12], value: i32) -> &[u8] {
    let negative = value < 0;
    // `unsigned_abs` so `i32::MIN` cannot overflow the negation.
    let mut magnitude = value.unsigned_abs();
    let mut end = digits.len();
    loop {
        end -= 1;
        digits[end] = b'0' + u8::try_from(magnitude % 10).unwrap_or(0);
        magnitude /= 10;
        if magnitude == 0 {
            break;
        }
    }
    if negative {
        end -= 1;
        digits[end] = b'-';
    }
    &digits[end..]
}

/// Write `bytes` to stderr, retrying short and interrupted writes.
///
/// `write(2)` is async-signal-safe; `eprintln!` is not (it locks and may
/// allocate), so the handler cannot use the usual machinery.
#[cfg(unix)]
fn write_stderr(bytes: &[u8]) {
    let mut offset = 0_usize;
    while offset < bytes.len() {
        // SAFETY: writing `len - offset` bytes from inside `bytes`, so the
        // pointer and length stay within the slice for the whole call.
        let written = unsafe {
            libc::write(
                libc::STDERR_FILENO,
                bytes[offset..].as_ptr().cast::<libc::c_void>(),
                bytes.len() - offset,
            )
        };
        if written > 0 {
            offset += written as usize;
            continue;
        }
        // A retryable interrupt loops; anything else means stderr is gone and
        // there is nowhere left to report to.
        let interrupted = written < 0
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
        if !interrupted {
            return;
        }
    }
}

/// Signal handler: name the signal on stderr, then exit `128 + signo`.
#[cfg(unix)]
extern "C" fn report_and_exit(signo: c_int) {
    let name = signal_name(signo).unwrap_or("UNKNOWN");
    let mut line = [0_u8; DIAGNOSTIC_BUF_BYTES];
    let len = render_diagnostic(&mut line, name, signo);
    write_stderr(&line[..len]);
    // SAFETY: `_exit` is async-signal-safe and never returns; skipping the
    // stdout flush is deliberate (no half-written result map).
    unsafe { libc::_exit(exit_code(signo)) }
}

/// Install the diagnostic handler for every signal in [`HANDLED_SIGNALS`].
///
/// Returns the names of any signals whose handler could not be installed, so
/// the caller can report the gap instead of silently running without it. An
/// empty vector means every handler is armed.
#[cfg(unix)]
pub fn install() -> Vec<&'static str> {
    let mut failed = Vec::new();
    for &(signo, name) in HANDLED_SIGNALS {
        // SAFETY: `action` is a zeroed `sigaction` with a valid handler
        // address and an initialised mask; `sigaction` only reads it.
        let installed = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = report_and_exit as *const () as libc::sighandler_t;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signo, &action, std::ptr::null_mut())
        };
        if installed != 0 {
            failed.push(name);
        }
    }
    failed
}

/// Non-Unix hosts have no POSIX signals to arm; nothing to install.
#[cfg(not(unix))]
pub fn install() -> Vec<&'static str> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_follows_the_128_plus_signo_convention() {
        // The GRQ-24 log's `exitCode=158` is macOS SIGUSR1 (30).
        assert_eq!(exit_code(30), 158);
        assert_eq!(exit_code(15), 143);
        assert_eq!(exit_code(2), 130);
    }

    #[cfg(unix)]
    #[test]
    fn handled_signals_map_to_their_names() {
        assert_eq!(signal_name(libc::SIGUSR1), Some("SIGUSR1"));
        assert_eq!(signal_name(libc::SIGTERM), Some("SIGTERM"));
        assert_eq!(signal_name(libc::SIGINT), Some("SIGINT"));
    }

    #[cfg(unix)]
    #[test]
    fn unhandled_signals_have_no_name() {
        assert_eq!(signal_name(libc::SIGKILL), None);
    }

    #[test]
    fn diagnostic_names_the_signal_the_cause_and_the_exit_code() {
        let mut buf = [0_u8; DIAGNOSTIC_BUF_BYTES];
        let len = render_diagnostic(&mut buf, "SIGUSR1", 30);
        let line = std::str::from_utf8(&buf[..len]).expect("diagnostic is UTF-8");
        assert_eq!(
            line,
            "[signal] rust_scorer terminated by SIGUSR1 (signal 30) — external termination, \
             not a scoring failure; no result JSON was produced; exiting 158\n"
        );
    }

    #[test]
    fn diagnostic_is_one_line() {
        let mut buf = [0_u8; DIAGNOSTIC_BUF_BYTES];
        let len = render_diagnostic(&mut buf, "SIGTERM", 15);
        let line = std::str::from_utf8(&buf[..len]).expect("diagnostic is UTF-8");
        assert_eq!(
            line.matches('\n').count(),
            1,
            "exactly one trailing newline"
        );
        assert!(line.ends_with('\n'));
    }

    #[cfg(unix)]
    #[test]
    fn every_handled_signal_fits_the_buffer() {
        for &(signo, name) in HANDLED_SIGNALS {
            let mut buf = [0_u8; DIAGNOSTIC_BUF_BYTES];
            let len = render_diagnostic(&mut buf, name, signo);
            assert!(
                len < DIAGNOSTIC_BUF_BYTES,
                "{name} fills the whole buffer — it may be truncated"
            );
            assert!(buf[..len].ends_with(b"\n"), "{name} line is truncated");
        }
    }

    #[test]
    fn a_short_buffer_truncates_instead_of_panicking() {
        let mut buf = [0_u8; 8];
        let len = render_diagnostic(&mut buf, "SIGTERM", 15);
        assert_eq!(len, 8);
        assert_eq!(&buf[..len], b"[signal]");
    }

    #[test]
    fn unknown_signal_numbers_still_render() {
        let mut buf = [0_u8; DIAGNOSTIC_BUF_BYTES];
        let len = render_diagnostic(&mut buf, "UNKNOWN", 0);
        let line = std::str::from_utf8(&buf[..len]).expect("diagnostic is UTF-8");
        assert!(line.contains("UNKNOWN (signal 0)"), "got: {line}");
        assert!(line.ends_with("exiting 128\n"), "got: {line}");
    }
}
