//! NEAT-AI-Lamarck#123 — a sampled directory call fetches only the records it
//! scores, and scores them **identically**.
//!
//! A screen call at `--sample-rate 0.05` used to read and decode the whole
//! corpus to throw 95 % of it away, which is why its fixed per-call cost was 5×
//! a full-corpus call's. Reading only the kept records removes that cost — but
//! it touches the authoritative scoring path, so a different number here is a
//! correctness bug, not a performance one.
//!
//! This test drives the real directory entrypoint twice over the same corpus at
//! the same rate — once with the sampled reader, once with it switched off via
//! `NEAT_SCORER_SAMPLED_READ=off` — and requires the two to agree **bit for
//! bit**, on every creature.

use std::io::Write;
use std::path::{Path, PathBuf};

use rust_scorer::cost::CostKind;
use rust_scorer::gpu::GpuBackendLabel;
use rust_scorer::multi_score::score_from_creature_dir_sampled;
use rust_scorer::sampling::SampleSpec;

/// Production record shape: 2 511 inputs + 1 target = 10 048 bytes, the size the
/// sampled-read policy is measured against.
const INPUTS: usize = 2511;
const OUTPUTS: usize = 1;
const RECORD_BYTES: usize = (INPUTS + OUTPUTS) * 4;
const RECORDS: usize = 400;

/// Forward-only creature reading both ends of the record, so a mis-selected
/// record cannot cancel out of the error.
fn creature(weight_first: f32, weight_last: f32) -> String {
    format!(
        r#"{{"input":{INPUTS},"output":{OUTPUTS},"forwardOnly":true,"neurons":[{{"type":"output","uuid":"output-0","bias":0.0,"squash":"IDENTITY"}}],"synapses":[{{"fromUUID":"input-0","toUUID":"output-0","weight":{weight_first}}},{{"fromUUID":"input-{}","toUUID":"output-0","weight":{weight_last}}}]}}"#,
        INPUTS - 1
    )
}

/// Record `r` carries `r` in its first input, `r/2` in its last, and target 0 —
/// so which records were read is visible in the error.
fn write_corpus(dir: &Path, files: usize) {
    std::fs::create_dir_all(dir).expect("create data dir");
    let mut global = 0_usize;
    for f in 0..files {
        let mut file = std::fs::File::create(dir.join(format!("{f}.bin"))).expect("create bin");
        for _ in 0..RECORDS {
            let mut values = vec![0.0_f32; INPUTS + OUTPUTS];
            values[0] = global as f32;
            values[INPUTS - 1] = global as f32 / 2.0;
            for v in &values {
                file.write_all(&v.to_le_bytes()).expect("write f32");
            }
            global += 1;
        }
    }
}

fn fixture() -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join("sampled_read_parity");
    let _ = std::fs::remove_dir_all(&root);
    let creatures = root.join("creatures");
    let data = root.join("data");
    std::fs::create_dir_all(&creatures).expect("create creatures dir");
    std::fs::write(creatures.join("creature-0.json"), creature(1.0, 0.0)).expect("write creature");
    std::fs::write(creatures.join("creature-1.json"), creature(0.25, 3.0)).expect("write creature");
    write_corpus(&data, 3);
    (creatures, data)
}

fn score_at(
    creatures: &Path,
    data: &Path,
    rate: f64,
    phase: u64,
) -> Vec<(String, f64, f64, usize)> {
    let scored = score_from_creature_dir_sampled(
        creatures,
        data,
        GpuBackendLabel::CpuFallback,
        CostKind::Mse,
        &SampleSpec::new(rate, phase).expect("valid sample spec"),
    )
    .expect("sampled score");
    let mut rows: Vec<(String, f64, f64, usize)> = scored
        .into_iter()
        .map(|(stem, r)| (stem, r.score, r.error, r.record_count))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

#[test]
fn a_sampled_read_scores_bit_identically_to_the_full_sweep() {
    // Small reads so the corpus is split across many windows and several
    // readers, which is where an ordering bug would show up.
    // SAFETY: set before the scorer spawns worker threads.
    unsafe { std::env::set_var("NEAT_SCORER_READ_BYTES", (RECORD_BYTES * 8).to_string()) };
    let (creatures, data) = fixture();

    for (rate, phase) in [(0.05, 0), (0.05, 11), (0.1, 3)] {
        // SAFETY: single-threaded section of this test; no scorer running.
        unsafe { std::env::remove_var("NEAT_SCORER_SAMPLED_READ") };
        let sampled = score_at(&creatures, &data, rate, phase);

        unsafe { std::env::set_var("NEAT_SCORER_SAMPLED_READ", "off") };
        let full = score_at(&creatures, &data, rate, phase);
        unsafe { std::env::remove_var("NEAT_SCORER_SAMPLED_READ") };

        assert_eq!(sampled.len(), 2, "both creatures must be scored");
        for ((stem, score, error, records), (full_stem, full_score, full_error, full_records)) in
            sampled.iter().zip(full.iter())
        {
            assert_eq!(stem, full_stem);
            assert_eq!(
                records, full_records,
                "{stem} at rate {rate} phase {phase} scored a different record count"
            );
            assert_eq!(
                score.to_bits(),
                full_score.to_bits(),
                "{stem} at rate {rate} phase {phase}: score {score} != {full_score}"
            );
            assert_eq!(
                error.to_bits(),
                full_error.to_bits(),
                "{stem} at rate {rate} phase {phase}: error {error} != {full_error}"
            );
        }
        // The sample really did cut the corpus down, so the comparison above is
        // not two full sweeps agreeing with each other.
        let expected = ((RECORDS * 3) as f64 * rate).floor() as usize;
        assert_eq!(sampled[0].3, expected, "rate {rate} scored the wrong count");
    }
}
