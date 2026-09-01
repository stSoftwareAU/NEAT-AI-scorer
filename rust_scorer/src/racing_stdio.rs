//! Line-delimited stdio racing protocol for directory-mode early exit
//! (Issue #308 surface; consumed by NEAT-AI#3928).
//!
//! [`score_from_creature_dir_with_early_exit`](crate::multi_score::score_from_creature_dir_with_early_exit)
//! is a **library** entrypoint: the callback that decides which creatures to
//! abandon has to be Rust code linked into this crate. Callers that drive the
//! binary as a subprocess — every NEAT-AI deployment does — had no way to
//! supply one, so the early-exit path was implemented and never reached.
//!
//! This module is that missing surface. Under `--race-stdio` the scorer writes
//! one JSON line per scored chunk to stdout and blocks reading exactly one
//! verdict line from stdin, so the caller's own policy makes the decision:
//!
//! ```text
//! scorer → caller  {"racing":"chunk","chunk":1,"partials":[{"index":0,"key":"a","partialError":0.51,"recordsScored":1024}]}
//! caller → scorer  {"verdict":"continue"}
//! caller → scorer  {"verdict":"abort","creatures":[0,2]}
//! caller → scorer  {"verdict":"abortAll"}
//! ```
//!
//! The final result map is printed after the sweep exactly as in plain
//! directory mode, so a caller reads chunk events until the stream stops
//! producing them and then parses what remains.
//!
//! **Fail loud, never silently full-score.** A closed stdin, a malformed
//! verdict, an unknown verdict, or a write failure aborts the sweep and makes
//! the process exit non-zero with the reason on stderr. Degrading to
//! "continue" would hand the caller a full-corpus score it never asked for —
//! cheap to mistake for a working race.

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

use crate::multi_score::{EarlyExit, PartialScore};

/// One creature's running score, as serialised on the wire.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PartialScoreLine<'a> {
    /// Creature index within the loaded directory (sorted file order). This is
    /// the value a verdict names in `creatures`.
    index: usize,
    /// Creature id — the `.json` file stem, matching the result-map key.
    key: &'a str,
    /// Mean error over the records scored so far.
    partial_error: f64,
    /// How many records this creature has been scored against so far.
    records_scored: usize,
}

/// One chunk event written to stdout.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChunkEvent<'a> {
    /// Constant discriminator so a caller can tell a racing event apart from
    /// the result JSON on the same stream.
    racing: &'static str,
    /// 1-based chunk counter, for diagnostics and protocol-desync detection.
    chunk: u64,
    /// One entry per still-active creature.
    partials: Vec<PartialScoreLine<'a>>,
}

/// Caller verdict read back from stdin after each chunk event.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "verdict", rename_all = "camelCase")]
enum Verdict {
    /// Keep scoring every active creature.
    Continue,
    /// Stop scoring the listed creature indices.
    Abort {
        /// Creature indices to abandon; unknown indices are ignored by the
        /// scoring loop.
        creatures: Vec<usize>,
    },
    /// Stop the sweep entirely.
    AbortAll,
}

/// Drives the racing protocol over an arbitrary reader/writer pair.
///
/// Generic over the streams so the protocol is testable without a subprocess;
/// [`RacingStdio::on_chunk`] is the closure handed to
/// `score_from_creature_dir_sampled_with_early_exit`.
pub struct RacingStdio<R: BufRead, W: Write> {
    reader: R,
    writer: W,
    chunk: u64,
    failure: Option<String>,
}

impl<R: BufRead, W: Write> RacingStdio<R, W> {
    /// Build a protocol driver over `reader` (verdicts in) and `writer`
    /// (chunk events out).
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            chunk: 0,
            failure: None,
        }
    }

    /// Handle one scored chunk: publish the partials, block for the verdict.
    ///
    /// On any protocol fault the reason is recorded and [`EarlyExit::AbortAll`]
    /// is returned so the sweep stops immediately; [`RacingStdio::failure`]
    /// then reports it to the caller, which turns it into a non-zero exit.
    pub fn on_chunk(&mut self, partials: &[PartialScore]) -> EarlyExit {
        if self.failure.is_some() {
            return EarlyExit::AbortAll;
        }
        match self.exchange(partials) {
            Ok(exit) => exit,
            Err(reason) => {
                self.failure = Some(reason);
                EarlyExit::AbortAll
            }
        }
    }

    /// The protocol fault that stopped the sweep, if any.
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    fn exchange(&mut self, partials: &[PartialScore]) -> Result<EarlyExit, String> {
        self.chunk += 1;
        let event = ChunkEvent {
            racing: "chunk",
            chunk: self.chunk,
            partials: partials
                .iter()
                .map(|p| PartialScoreLine {
                    index: p.creature_index,
                    key: &p.key,
                    partial_error: p.partial_error,
                    records_scored: p.records_scored,
                })
                .collect(),
        };
        let line = serde_json::to_string(&event)
            .map_err(|e| format!("--race-stdio: failed to serialise chunk event: {e}"))?;
        writeln!(self.writer, "{line}")
            .map_err(|e| format!("--race-stdio: failed to write chunk event: {e}"))?;
        self.writer
            .flush()
            .map_err(|e| format!("--race-stdio: failed to flush chunk event: {e}"))?;

        let mut reply = String::new();
        let read = self
            .reader
            .read_line(&mut reply)
            .map_err(|e| format!("--race-stdio: failed to read verdict: {e}"))?;
        if read == 0 {
            return Err(format!(
                "--race-stdio: verdict stream closed after chunk {} (expected one verdict per chunk)",
                self.chunk
            ));
        }
        let trimmed = reply.trim();
        if trimmed.is_empty() {
            return Err(format!(
                "--race-stdio: empty verdict line after chunk {}",
                self.chunk
            ));
        }
        let verdict: Verdict = serde_json::from_str(trimmed).map_err(|e| {
            format!(
                "--race-stdio: unparseable verdict after chunk {}: {e} (line: {trimmed})",
                self.chunk
            )
        })?;
        Ok(match verdict {
            Verdict::Continue => EarlyExit::Continue,
            Verdict::Abort { creatures } => EarlyExit::AbortCreatures(creatures),
            Verdict::AbortAll => EarlyExit::AbortAll,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn partial(index: usize, key: &str, error: f64, records: usize) -> PartialScore {
        PartialScore {
            creature_index: index,
            key: key.to_string(),
            partial_error: error,
            records_scored: records,
        }
    }

    fn drive(verdicts: &str, partials: &[PartialScore]) -> (EarlyExit, String, Option<String>) {
        let mut driver = RacingStdio::new(Cursor::new(verdicts.as_bytes().to_vec()), Vec::new());
        let exit = driver.on_chunk(partials);
        let failure = driver.failure().map(str::to_string);
        let written = String::from_utf8(driver.writer).expect("utf8 chunk event");
        (exit, written, failure)
    }

    #[test]
    fn publishes_one_chunk_event_and_returns_continue() {
        let (exit, written, failure) = drive(
            "{\"verdict\":\"continue\"}\n",
            &[partial(0, "alpha", 0.25, 128)],
        );
        assert_eq!(exit, EarlyExit::Continue);
        assert_eq!(failure, None);
        let parsed: serde_json::Value =
            serde_json::from_str(written.trim()).expect("chunk event is one JSON line");
        assert_eq!(parsed["racing"], "chunk");
        assert_eq!(parsed["chunk"], 1);
        assert_eq!(parsed["partials"][0]["index"], 0);
        assert_eq!(parsed["partials"][0]["key"], "alpha");
        assert_eq!(parsed["partials"][0]["partialError"], 0.25);
        assert_eq!(parsed["partials"][0]["recordsScored"], 128);
    }

    #[test]
    fn abort_verdict_maps_to_abort_creatures() {
        let (exit, _, failure) = drive(
            "{\"verdict\":\"abort\",\"creatures\":[2,5]}\n",
            &[partial(0, "a", 0.1, 8)],
        );
        assert_eq!(exit, EarlyExit::AbortCreatures(vec![2, 5]));
        assert_eq!(failure, None);
    }

    #[test]
    fn abort_all_verdict_stops_the_sweep() {
        let (exit, _, failure) = drive("{\"verdict\":\"abortAll\"}\n", &[partial(0, "a", 0.1, 8)]);
        assert_eq!(exit, EarlyExit::AbortAll);
        assert_eq!(failure, None);
    }

    #[test]
    fn closed_verdict_stream_fails_loud() {
        let (exit, _, failure) = drive("", &[partial(0, "a", 0.1, 8)]);
        assert_eq!(exit, EarlyExit::AbortAll);
        let failure = failure.expect("closed stdin must be recorded as a failure");
        assert!(
            failure.contains("verdict stream closed"),
            "unexpected failure: {failure}"
        );
    }

    #[test]
    fn unparseable_verdict_fails_loud() {
        let (exit, _, failure) = drive("not json\n", &[partial(0, "a", 0.1, 8)]);
        assert_eq!(exit, EarlyExit::AbortAll);
        let failure = failure.expect("malformed verdict must be recorded as a failure");
        assert!(
            failure.contains("unparseable verdict"),
            "unexpected failure: {failure}"
        );
    }

    #[test]
    fn unknown_verdict_name_fails_loud_rather_than_continuing() {
        let (exit, _, failure) = drive("{\"verdict\":\"carryOn\"}\n", &[partial(0, "a", 0.1, 8)]);
        assert_eq!(exit, EarlyExit::AbortAll);
        assert!(
            failure.is_some(),
            "unknown verdict must not be a silent continue"
        );
    }

    #[test]
    fn a_recorded_failure_keeps_aborting_without_reading_more() {
        let mut driver = RacingStdio::new(Cursor::new(Vec::new()), Vec::new());
        assert_eq!(
            driver.on_chunk(&[partial(0, "a", 0.1, 8)]),
            EarlyExit::AbortAll
        );
        let first = driver.failure().map(str::to_string);
        assert_eq!(
            driver.on_chunk(&[partial(0, "a", 0.1, 16)]),
            EarlyExit::AbortAll
        );
        assert_eq!(driver.failure().map(str::to_string), first);
    }
}
