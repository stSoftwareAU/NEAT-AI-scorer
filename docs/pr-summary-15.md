## Summary

Adds a `--creature-stdin` input mode to `rust_scorer` so callers can pipe the
creature JSON over stdin instead of writing it to a temp file. This unblocks
integration in restricted worker/sandbox environments where `Deno.makeTempFile`
(or similar) can fail even when write permission appears granted — the caller
no longer needs to own a temp file at all.

The existing positional contract (`<creature.json> <data_dir>`) is unchanged:
the flag is purely additive. Closes #15.

## Evidence

CLI-only change — no UI to screenshot. Behaviour is verified by:

* Unit tests exercising `resolve_inputs` and `score_from_json` directly (stdin
  mode parity, argument-count validation, invalid-JSON handling, clap parsing
  for both modes).
* An end-to-end smoke test (`scorer_binary_accepts_creature_via_stdin`) that
  spawns the compiled binary with `--creature-stdin`, pipes the identity
  fixture JSON to its stdin, and asserts the same near-zero `error` and
  `recordCount: 4` result as the file-mode smoke test.
* `./quality.sh` passes cleanly (shellcheck, cargo-deny, fmt, clippy, check,
  build, test, rustdoc with `-D warnings`, release build).

## Test Plan

Unit tests added in `rust_scorer/src/main.rs`:

* `test_stdin_mode_matches_file_mode` — file and stdin paths yield the same
  `ScoreResult`.
* `test_stdin_mode_rejects_extra_positional_args` — validation fires before
  any stdin read.
* `test_default_mode_requires_two_positional_args` — default mode still
  requires the `<creature.json> <data_dir>` pair.
* `test_score_from_json_rejects_invalid_json` — invalid JSON on the stdin
  path returns an error instead of panicking.
* `test_cli_parsing_both_modes` — clap accepts both argument shapes.

Integration test added in `rust_scorer/tests/scorer_smoke.rs`:

* `scorer_binary_accepts_creature_via_stdin` — end-to-end binary invocation
  with the creature JSON piped over stdin.

Existing tests updated only to match the refactored `Cli` shape (a
`creature_stdin: bool` flag plus a `Vec<PathBuf>` of positional args); no test
assertions were weakened or removed.
