//! Shared parsing for the numeric `NEAT_SCORER_*` performance-tuning env vars.
//!
//! These knobs previously fell back to their default on a malformed value with
//! **zero** feedback, so a typo like `NEAT_SCORER_READ_BYTES=2MB` was silently
//! ignored. [`parse_tuning_var`] distinguishes *unset/blank* (silent default)
//! from *set-but-malformed* (one stderr diagnostic + default), matching the
//! behaviour of `NEAT_SCORER_GPU` which already rejects invalid values
//! (Issue #204).

/// Parse a raw env-var value into `T`, reporting malformed input.
///
/// Returns the resolved value plus an optional warning message. The warning is
/// `Some` **only** when `raw` is present and non-blank yet fails to parse — a
/// value the user clearly intended to set but got wrong. An unset (`None`) or
/// blank value falls back to `default` silently, preserving the "knob is
/// optional" contract.
///
/// The caller is responsible for emitting the warning (so this stays pure and
/// unit-testable without touching process state or capturing stderr).
pub fn parse_tuning_var<T, F>(
    name: &str,
    raw: Option<&str>,
    default: T,
    parse: F,
) -> (T, Option<String>)
where
    T: std::fmt::Display,
    F: FnOnce(&str) -> Option<T>,
{
    let Some(raw) = raw else {
        return (default, None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (default, None);
    }
    match parse(trimmed) {
        Some(value) => (value, None),
        None => {
            let warning =
                format!("[scorer] ignoring invalid {name}='{raw}', using default {default}");
            (default, Some(warning))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_usize(s: &str) -> Option<usize> {
        s.parse::<usize>().ok()
    }

    #[test]
    fn unset_value_uses_default_silently() {
        let (value, warning) =
            parse_tuning_var("NEAT_SCORER_READ_BYTES", None, 42usize, parse_usize);
        assert_eq!(value, 42);
        assert!(warning.is_none(), "unset must not warn");
    }

    #[test]
    fn blank_value_uses_default_silently() {
        for raw in ["", "   ", "\t\n"] {
            let (value, warning) =
                parse_tuning_var("NEAT_SCORER_READ_BYTES", Some(raw), 42usize, parse_usize);
            assert_eq!(value, 42);
            assert!(warning.is_none(), "blank {raw:?} must not warn");
        }
    }

    #[test]
    fn valid_value_is_honoured_silently() {
        let (value, warning) =
            parse_tuning_var("NEAT_SCORER_READ_BYTES", Some("4096"), 42usize, parse_usize);
        assert_eq!(value, 4096);
        assert!(warning.is_none(), "valid value must not warn");
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_before_parsing() {
        let (value, warning) = parse_tuning_var(
            "NEAT_SCORER_READ_BYTES",
            Some("  4096  "),
            42usize,
            parse_usize,
        );
        assert_eq!(value, 4096);
        assert!(warning.is_none());
    }

    #[test]
    fn malformed_value_warns_and_falls_back() {
        let (value, warning) =
            parse_tuning_var("NEAT_SCORER_READ_BYTES", Some("2MB"), 42usize, parse_usize);
        assert_eq!(value, 42, "must fall back to the default");
        let warning = warning.expect("malformed value must warn");
        assert!(warning.contains("NEAT_SCORER_READ_BYTES"));
        assert!(warning.contains("2MB"), "warning echoes the raw value");
        assert!(warning.contains("42"), "warning echoes the default");
        assert!(warning.starts_with("[scorer] ignoring invalid"));
    }

    #[test]
    fn warning_preserves_untrimmed_raw_for_context() {
        // The echoed raw keeps surrounding whitespace so the user sees exactly
        // what they set.
        let (_value, warning) = parse_tuning_var(
            "NEAT_SCORER_READ_BYTES",
            Some(" oops "),
            7usize,
            parse_usize,
        );
        let warning = warning.expect("malformed value must warn");
        assert!(warning.contains("' oops '"));
    }

    #[test]
    fn custom_predicate_rejection_warns() {
        // A value that parses but is semantically invalid (e.g. zero budget)
        // must also warn, not fall back silently.
        let parse_positive = |s: &str| s.parse::<u64>().ok().filter(|&b| b > 0);
        let (value, warning) = parse_tuning_var(
            "NEAT_SCORER_GPU_SCRATCH_BYTES",
            Some("0"),
            99u64,
            parse_positive,
        );
        assert_eq!(value, 99);
        let warning = warning.expect("zero must warn");
        assert!(warning.contains("='0'"));
    }
}
