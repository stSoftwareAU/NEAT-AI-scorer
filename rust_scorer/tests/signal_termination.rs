//! External termination must be loud, not silent (Issue #591).
//!
//! A GRQ-24 sampler run hit its 3-hour per-task cap while `rust_scorer` was
//! mid-batch. `run_core` signalled the process group, the scorer died on the
//! **default** disposition, and the only thing the NEAT-AI batch bridge could
//! see was
//!
//! ```text
//! [scorer-strict] no-named-creature reason=EXEC_FAILURE exitCode=158
//! ScorerStrictError: Rust scorer batch call failed (exit 158) for 20 creature(s)
//! --- rust_scorer stderr ---
//! [gpu] auto fallback to CPU directory mode: …
//! ```
//!
//! — an exit code and an unrelated GPU note. Nothing said the scorer had been
//! killed from outside, so an environmental event read as a scoring failure.
//!
//! These tests drive the compiled binary, stop it mid-sweep with a real signal
//! and assert the contract: one grep-able `[signal] …` line on stderr, and the
//! conventional `128 + signo` exit status.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const INPUTS: usize = 2;
const OUTPUTS: usize = 1;
const RECORD_BYTES: usize = (INPUTS + OUTPUTS) * 4;
const RECORDS: usize = 4096;

/// Forward-only creature computing `w0*x0 + w1*x1` into one IDENTITY output.
fn linear_creature(w0: f32, w1: f32) -> String {
    format!(
        r#"{{"input":{INPUTS},"output":{OUTPUTS},"forwardOnly":true,"neurons":[{{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}}],"synapses":[{{"fromUUID":"input-0","toUUID":"output-0","weight":{w0}}},{{"fromUUID":"input-1","toUUID":"output-0","weight":{w1}}}]}}"#
    )
}

/// Build a `creatures/` + `data/` fixture pair under a fresh temp root.
fn fixture(tag: &str) -> (PathBuf, PathBuf, tempfile::TempDir) {
    let root = tempfile::Builder::new()
        .prefix(&format!("signal_termination_{tag}_"))
        .tempdir()
        .expect("create temp root");
    let creatures = root.path().join("creatures");
    let data = root.path().join("data");
    std::fs::create_dir_all(&creatures).expect("create creatures dir");
    std::fs::create_dir_all(&data).expect("create data dir");
    std::fs::write(
        creatures.join("creature-000.json"),
        linear_creature(0.4, 0.5),
    )
    .expect("write creature");
    let mut file = std::fs::File::create(data.join("0.bin")).expect("create data file");
    for r in 0..RECORDS {
        let x0 = (r as f32 * 0.01).sin();
        let x1 = (r as f32 * 0.017).cos();
        let target = 0.4 * x0 + 0.5 * x1;
        for v in [x0, x1, target] {
            file.write_all(&v.to_le_bytes()).expect("write f32");
        }
    }
    (creatures, data, root)
}

/// Start the scorer in `--race-stdio` mode and block it mid-sweep.
///
/// The racing protocol publishes one chunk event and then blocks reading a
/// verdict from stdin, so reading that first event proves the process is alive
/// and parked inside a scoring run — no sleeps, no polling, no flakiness.
fn scorer_parked_mid_sweep(
    creatures: &Path,
    data: &Path,
) -> (Child, BufReader<std::process::ChildStdout>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rust_scorer"))
        .arg("--gpu")
        .arg("off")
        .arg("--race-stdio")
        .arg(creatures)
        .arg(data)
        .env("NEAT_SCORER_READ_BYTES", RECORD_BYTES.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn racing scorer");

    let mut stdout = BufReader::new(child.stdout.take().expect("racing stdout"));
    let mut line = String::new();
    stdout.read_line(&mut line).expect("read first chunk event");
    assert!(
        line.starts_with("{\"racing\""),
        "expected a chunk event first, got: {line}"
    );
    (child, stdout)
}

/// Outcome of signalling a parked scorer.
struct SignalledRun {
    exit_code: Option<i32>,
    stderr: String,
}

/// Park the scorer mid-sweep, deliver `signo`, and collect how it died.
fn signal_parked_scorer(tag: &str, signo: i32) -> SignalledRun {
    let (creatures, data, _root) = fixture(tag);
    let (mut child, _stdout) = scorer_parked_mid_sweep(&creatures, &data);

    // SAFETY: `kill` on a live child pid we own; the pid cannot have been
    // reaped because `child` has not been waited on.
    let sent = unsafe { libc::kill(child.id() as libc::pid_t, signo) };
    assert_eq!(sent, 0, "failed to signal the scorer");

    // Reading to EOF returns when the scorer's stderr pipe closes, i.e. when it
    // is gone — so this waits for the death it just triggered.
    let mut stderr = String::new();
    std::io::Read::read_to_string(
        &mut child.stderr.take().expect("racing stderr"),
        &mut stderr,
    )
    .expect("read scorer stderr");

    let status = child.wait().expect("await signalled scorer");
    SignalledRun {
        exit_code: status.code(),
        stderr,
    }
}

#[test]
fn sigusr1_mid_sweep_names_itself_on_stderr() {
    let run = signal_parked_scorer("sigusr1", libc::SIGUSR1);
    assert!(
        run.stderr.contains("[signal]"),
        "external termination must leave a grep-able marker, got: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("SIGUSR1"),
        "the diagnostic must name the signal, got: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("not a scoring failure"),
        "the diagnostic must say this is not a scoring failure, got: {}",
        run.stderr
    );
}

#[test]
fn sigusr1_mid_sweep_exits_128_plus_signo() {
    let run = signal_parked_scorer("sigusr1_code", libc::SIGUSR1);
    assert_eq!(
        run.exit_code,
        Some(128 + libc::SIGUSR1),
        "the conventional signal exit status must be preserved; stderr: {}",
        run.stderr
    );
}

#[test]
fn sigterm_mid_sweep_names_itself_on_stderr() {
    let run = signal_parked_scorer("sigterm", libc::SIGTERM);
    assert!(
        run.stderr.contains("SIGTERM"),
        "the diagnostic must name the signal, got: {}",
        run.stderr
    );
    assert_eq!(
        run.exit_code,
        Some(128 + libc::SIGTERM),
        "the conventional signal exit status must be preserved; stderr: {}",
        run.stderr
    );
}

#[test]
fn signalled_run_prints_no_partial_result_json() {
    let (creatures, data, _root) = fixture("no_partial_json");
    let (mut child, mut stdout) = scorer_parked_mid_sweep(&creatures, &data);
    // SAFETY: as above — a live, unreaped child pid.
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) },
        0,
        "failed to signal the scorer"
    );
    child.wait().expect("await signalled scorer");

    let mut tail = String::new();
    use std::io::Read;
    stdout.read_to_string(&mut tail).ok();
    for line in tail.lines().filter(|l| !l.starts_with("{\"racing\"")) {
        assert!(
            line.trim().is_empty(),
            "a killed sweep must not emit a result map, got: {line}"
        );
    }
}
