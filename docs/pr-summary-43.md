# Enable Profile-Guided Optimisation (PGO) for release build

## Summary

Adds an opt-in PGO build flow for `rust_scorer` so the release binary can
benefit from feedback-directed code layout, inlining, and branch prediction
on top of the existing LTO + `codegen-units = 1` settings. The flow is
driven by a new `scripts/build-pgo.sh` helper that wraps the manual
`-Cprofile-generate` / `-Cprofile-use` `rustc` flow — no `cargo-pgo`
install required. A new `pgo` Cargo profile keeps the two compiler passes
identical to `release`. Closes #43.

## Changes

- **`scripts/build-pgo.sh`** — new helper that:
  1. generates a deterministic synthetic training fixture (Python),
  2. builds an instrumented binary with `RUSTFLAGS=-Cprofile-generate=…`,
  3. runs it against the fixture in both single-creature and directory
     mode to gather `*.profraw`,
  4. merges them with `llvm-profdata merge`,
  5. re-builds with `RUSTFLAGS=-Cprofile-use=…/merged.profdata`.
- **`Cargo.toml`** — new `[profile.pgo]` (inherits `release`) plus
  per-package `codegen-units = 1` so the two PGO passes match the
  release profile exactly.
- **`tests/scripts/build_pgo.bats`** — eight bats tests that shim
  `cargo`, `llvm-profdata`, and `rustc` to verify the orchestration end
  to end (instrumented build first, two scoring runs, merge, final build
  with `-Cprofile-use`, error propagation, env overrides).
- **`README.md`** — new "Optimised release build (PGO)" section
  documenting prerequisites, tunables, benchmark evidence, and the
  workflow-OAuth-scope blocker for adding a CI artefact job.
- **`docs/evidence/pgo-bench-300mb.log`** — captured timing data from
  the reproduced benchmark.

## CI workflow

The issue mentions adding a CI workflow that produces the PGO binary as
a release artefact. The worker is not authorised to push workflow YAML
changes (no `workflow` OAuth scope per `AGENTS.md` "Human Escalation"),
so the PR follows the issue's fallback: the manual flow is fully
documented in `README.md` and no workflow file is added. A maintainer
can wire `scripts/build-pgo.sh` into a manually triggered workflow if
desired.

## Evidence

This is a performance change. Following the issue's acceptance criterion
(`≥ 3 % improvement on at least one of the two scoring benches`), the
binary was timed against an identical 300 MB synthetic corpus, 15 timed
runs each (median / best, lower is better) on Apple silicon. Raw output
in `docs/evidence/pgo-bench-300mb.log`:

| Scenario | release median | PGO median | Δ median |
|---|---:|---:|---:|
| `score_from_json_fused` (single-creature) | 447.6 ms | 407.7 ms | **−8.9 %** |
| `score_from_creature_dir` (10 creatures) | 2079.2 ms | 1911.2 ms | **−8.1 %** |

Both scoring paths beat the 3 % threshold. The single-creature path is
noisier (smaller fraction of wall-clock spent inside the inner loops),
so additional runs on a 100 MB fixture were also collected during
development; the directory-mode improvement was consistent across all
runs while the single-creature improvement landed in the +5–12 % band
once the corpus was large enough to dominate CLI start-up.

## Test plan

- `bats tests/scripts/build_pgo.bats` — 8/8 pass
- `./quality.sh` — passes cleanly (shellcheck, fmt, clippy, tests, doc,
  release build).
- Manual end-to-end run of `./scripts/build-pgo.sh` produced the binary
  at `target/pgo/rust_scorer` and the merged profile at
  `target/pgo-profiles/merged.profdata`.

## Notes for reviewers

- `scripts/build-pgo.sh` is shellcheck-clean and locates `llvm-profdata`
  via three paths in priority order: `LLVM_PROFDATA` env override →
  `command -v` on PATH → `rustc --print sysroot`/`lib/rustlib`. The
  rustup-shipped binary is not on PATH by default, so the sysroot
  fallback is the common case after `rustup component add llvm-tools`.
- The bats tests deliberately don't exercise the real cargo build;
  Criterion / PGO cycles take minutes and aren't deterministic. The
  shim approach mirrors the existing `tests/scripts/run_benches.bats`
  pattern.
