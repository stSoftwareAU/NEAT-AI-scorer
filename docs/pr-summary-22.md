## Summary

Tunes the codespell setup so CI and contributors share a single source of truth and can reproduce the spell check locally. Adds a `.codespellrc` config file with a curated ignore list, a new `scripts/spell-check.sh` preflight script, and wires the preflight into `./quality.sh`. CI now installs codespell and calls the same preflight, so CI and local runs stay in lock-step. Closes #22.

### What changed

- **`.codespellrc`** — shared codespell config. Curated ignore list (`renderD`, `mape`, `MAPE`) lives here with justification comments, plus `skip` paths (`./target`, `./.git`, `./Cargo.lock`, bats fixture file) and the `check-filenames` / `check-hidden` flags previously duplicated in CI.
- **`scripts/spell-check.sh`** — local preflight script. Resolves user-site codespell installs (macOS / CI), fails fast with install instructions when missing, `cd`s into the target root so `.codespellrc` is picked up automatically. Supports `--root <dir>` for tests.
- **`quality.sh`** — now invokes `scripts/spell-check.sh` alongside the other preflight checks. Running `./quality.sh` reproduces the CI spell-check job.
- **`.github/workflows/ci.yml`** — the spell-check job now installs codespell and runs `scripts/spell-check.sh`, replacing the duplicated `ignore_words_list` / `check_filenames` / `skip` inputs. Single source of truth in `.codespellrc`.
- **`README.md`** — new "Spell check" subsection under Build documents the workflow for running the preflight, extending the ignore list, and what the current curated entries mean.
- **`tests/scripts/spell_check.bats`** — new bats suite exercising the preflight end-to-end (clean tree, genuine typo, curated ignores, hidden files, target/ skip, unknown flag, real repo).

### Acceptance criteria

- ✅ Known valid domain terms (`renderD`, `mape`, `MAPE`) no longer cause recurring CI noise — bats test `ignores curated domain terms` verifies.
- ✅ New genuine typos still fail CI — bats test `fails on a genuine typo` verifies (`sentance` is flagged).
- ✅ Local command path exists to reproduce CI — `scripts/spell-check.sh` is invoked by both CI and `quality.sh`; bats test `real repository passes the spell check` confirms.

## Evidence

Backend / CI change — no UI to screenshot. Verified via:

1. `./quality.sh < /dev/null` — passes cleanly, including the new `📝 Running codespell preflight (mirrors CI spell-check job)…` step.
2. `bats tests/scripts/spell_check.bats` — 7/7 tests passing.
3. Direct run: `./scripts/spell-check.sh` → `codespell: no typos found`.

## Test Plan

- Added `tests/scripts/spell_check.bats` covering:
  - Passes on a clean tree.
  - Fails on a genuine typo (`sentance`).
  - Ignores curated domain terms (`MAPE` / `mape` / `renderD`).
  - Scans hidden files and filenames (catches `recieve` in `.hidden`).
  - Skips the `target/` build directory.
  - Unknown flag prints usage and exits non-zero.
  - Real repository passes the spell check.
- Re-ran existing bats suite (`tests/scripts/version_increment.bats`, `tests/scripts/workflow_neat_ai_core_path.bats`) — still passing.
- Full `./quality.sh` gate re-run: fmt / clippy / check / build / test / doc / release all green.
