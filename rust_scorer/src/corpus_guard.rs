//! Pre-flight record-alignment guard for a `.bin` training corpus (Issue #476).
//!
//! The native scorer streams **every** `.bin` file as one continuous byte
//! stream (`neat_core::training_bin_stream::for_each_read_chunk` feeding
//! `crate::stream_io::run_io_loop`). The `pending` buffer carries a short
//! tail from file N straight into file N+1, so a single file whose length is
//! not a whole multiple of `record_bytes` makes the scorer **splice** a bogus
//! record across the file boundary and shift every record after it. The only
//! existing complaint arrives at the very end of the run
//! (`Trailing N bytes (incomplete record) after reading all training files`),
//! which names no file — and it misses the case entirely when two
//! misalignments cancel out across the corpus.
//!
//! The WASM/JS scorer in `@stsoftware/neat-ai`
//! (`src/creature/CreatureActivation.ts::evaluateDir`) frames records
//! **per file** and asserts on misalignment, so on a misaligned corpus the two
//! engines score different record sets — a small, systematic, one-direction
//! score offset between fleet hosts running native and hosts that silently
//! fell back to WASM.
//!
//! [`assert_records_aligned`] closes that gap: it is called immediately after
//! every `find_bin_files(...)` site, before any streaming starts, and fails
//! loudly naming the offending file. The end-of-stream trailing-bytes checks
//! stay in place as a backstop.

use std::fs;
use std::path::{Path, PathBuf};

/// Verify that every training `.bin` file holds a whole number of records.
///
/// Each file's byte length must be an exact multiple of `record_bytes`
/// (`(num_inputs + num_outputs) * 4`, i.e. `TrainingDataConfig::bytes_per_record`).
/// A file that is not aligned would have its short tail spliced onto the head
/// of the next file by the continuous-stream reader, silently shifting every
/// subsequent record.
///
/// # Errors
///
/// Returns `Err` with a human-readable message when:
///
/// * `record_bytes` is zero (the caller has a degenerate creature shape) —
///   reported rather than dividing by zero;
/// * any file's metadata cannot be read (a missing or unreadable file is an
///   error, never silently skipped);
/// * any file's size is not a whole multiple of `record_bytes`. The message
///   names the first offending file, its size and the remainder, and states
///   how many other files are also misaligned.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
/// use rust_scorer::corpus_guard::assert_records_aligned;
///
/// // An empty corpus is trivially aligned — emptiness is reported elsewhere.
/// assert!(assert_records_aligned(&[], 16).is_ok());
///
/// // A zero-width record is rejected rather than dividing by zero.
/// let files = vec![PathBuf::from("does-not-matter.bin")];
/// assert!(assert_records_aligned(&files, 0).is_err());
/// ```
pub fn assert_records_aligned(bin_files: &[PathBuf], record_bytes: usize) -> Result<(), String> {
    if record_bytes == 0 {
        return Err(
            "Invalid record size: record_bytes is zero, so training files cannot be framed \
             into records (num_inputs + num_outputs must be positive)"
                .to_string(),
        );
    }

    let record_bytes_u64 = record_bytes as u64;
    let mut first_misaligned: Option<(PathBuf, u64, u64)> = None;
    let mut misaligned_count: usize = 0;

    for path in bin_files {
        let len = file_len(path)?;
        let remainder = len % record_bytes_u64;
        if remainder != 0 {
            misaligned_count += 1;
            if first_misaligned.is_none() {
                first_misaligned = Some((path.clone(), len, remainder));
            }
        }
    }

    match first_misaligned {
        None => Ok(()),
        Some((path, len, remainder)) => {
            let mut message = format!(
                "Training file {} has size {} bytes, which is not a whole multiple of the \
                 {}-byte record size ({} bytes past the last whole record); records would be \
                 spliced across file boundaries",
                path.display(),
                len,
                record_bytes,
                remainder
            );
            let others = misaligned_count.saturating_sub(1);
            if others > 0 {
                message.push_str(&format!(
                    " ({others} other training {} also misaligned)",
                    if others == 1 { "file is" } else { "files are" }
                ));
            }
            Err(message)
        }
    }
}

/// Read a file's byte length, turning any I/O error into a descriptive message.
///
/// Deliberately propagates rather than skipping: an unreadable training file
/// means the corpus cannot be verified, and a silently skipped file is exactly
/// the class of bug this guard exists to prevent.
fn file_len(path: &Path) -> Result<u64, String> {
    fs::metadata(path)
        .map(|meta| meta.len())
        .map_err(|e| format!("Failed to read training file {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    const RECORD_BYTES: usize = 64;

    /// Write `name` with exactly `len` bytes of filler and return its path.
    fn write_file(dir: &TempDir, name: &str, len: usize) -> PathBuf {
        let path = dir.path().join(name);
        let mut file = File::create(&path).expect("create fixture file");
        file.write_all(&vec![0u8; len]).expect("write fixture file");
        file.sync_all().expect("flush fixture file");
        path
    }

    #[test]
    fn aligned_files_pass() {
        let dir = TempDir::new().expect("temp dir");
        let files = vec![
            write_file(&dir, "a.bin", RECORD_BYTES * 3),
            write_file(&dir, "b.bin", RECORD_BYTES),
            // A zero-length file is a whole (empty) number of records.
            write_file(&dir, "c.bin", 0),
        ];

        assert_eq!(assert_records_aligned(&files, RECORD_BYTES), Ok(()));
    }

    #[test]
    fn single_misaligned_file_is_rejected_and_named() {
        let dir = TempDir::new().expect("temp dir");
        let files = vec![
            write_file(&dir, "good.bin", RECORD_BYTES * 2),
            write_file(&dir, "ragged.bin", RECORD_BYTES * 2 + 17),
        ];

        let err = assert_records_aligned(&files, RECORD_BYTES)
            .expect_err("misaligned file must be rejected");

        assert!(
            err.contains("ragged.bin"),
            "error must name the file: {err}"
        );
        assert!(
            !err.contains("good.bin"),
            "error must name the offending file only: {err}"
        );
        assert!(
            err.contains(&format!("{} bytes", RECORD_BYTES * 2 + 17)),
            "error must state the file size: {err}"
        );
        assert!(
            err.contains("17 bytes past the last whole record"),
            "error must state the remainder: {err}"
        );
        assert!(
            err.contains(&format!("{RECORD_BYTES}-byte record size")),
            "error must state the record size: {err}"
        );
    }

    /// The case the end-of-stream trailing-bytes check misses entirely: two
    /// files whose misalignments cancel out, so the concatenated corpus is a
    /// whole number of records while every record after the first boundary is
    /// spliced from two different files.
    #[test]
    fn offsetting_misalignments_are_still_rejected() {
        let dir = TempDir::new().expect("temp dir");
        let short = RECORD_BYTES + 48;
        let long = 2 * RECORD_BYTES - 48;
        let files = vec![
            write_file(&dir, "first.bin", short),
            write_file(&dir, "second.bin", long),
        ];

        // Pre-condition: the concatenated stream *is* record-aligned, so the
        // existing trailing-bytes backstop would report nothing at all.
        assert_eq!((short + long) % RECORD_BYTES, 0);

        let err = assert_records_aligned(&files, RECORD_BYTES)
            .expect_err("offsetting misalignments must still be rejected");

        assert!(
            err.contains("first.bin"),
            "error must name the first offender: {err}"
        );
        assert!(
            err.contains("1 other training file is also misaligned"),
            "error must count the remaining offenders: {err}"
        );
    }

    #[test]
    fn several_misaligned_files_are_counted() {
        let dir = TempDir::new().expect("temp dir");
        let files = vec![
            write_file(&dir, "a.bin", RECORD_BYTES + 1),
            write_file(&dir, "b.bin", RECORD_BYTES + 2),
            write_file(&dir, "c.bin", RECORD_BYTES + 3),
        ];

        let err =
            assert_records_aligned(&files, RECORD_BYTES).expect_err("misalignment must be flagged");

        assert!(err.contains("a.bin"), "error must name the first: {err}");
        assert!(
            err.contains("2 other training files are also misaligned"),
            "error must count the remaining offenders: {err}"
        );
    }

    #[test]
    fn zero_record_bytes_is_rejected() {
        let dir = TempDir::new().expect("temp dir");
        let files = vec![write_file(&dir, "a.bin", 128)];

        let err = assert_records_aligned(&files, 0).expect_err("zero record size must be rejected");

        assert!(
            err.contains("record_bytes is zero"),
            "error must explain the zero record size: {err}"
        );
    }

    #[test]
    fn missing_file_surfaces_an_error() {
        let dir = TempDir::new().expect("temp dir");
        let present = write_file(&dir, "present.bin", RECORD_BYTES);
        let absent = dir.path().join("absent.bin");
        let files = vec![present, absent];

        let err = assert_records_aligned(&files, RECORD_BYTES)
            .expect_err("a missing file must not be skipped");

        assert!(
            err.contains("absent.bin"),
            "error must name the unreadable file: {err}"
        );
        assert!(
            err.contains("Failed to read training file"),
            "error must report the I/O failure: {err}"
        );
    }

    #[test]
    fn empty_corpus_is_aligned() {
        assert_eq!(assert_records_aligned(&[], RECORD_BYTES), Ok(()));
    }
}
