# Contributing to NEAT-AI-scorer

Thanks for your interest in improving **NEAT-AI-scorer** — the native MSE
scorer CLI for NEAT-AI creatures. This guide summarises how to build, test,
and submit changes. It mirrors the local gate documented in
[`AGENTS.md`](./AGENTS.md) and the CI workflow in
[`.github/workflows/ci.yml`](./.github/workflows/ci.yml).

## Repository layout

This is a multi-binary Rust workspace. The sole workspace member is
**`rust_scorer`** (the `rust_scorer`, `float_scan_bench`, and
`cost_scan_bench` binaries). The shared scoring logic lives in
**`neat-core`**, resolved as a **path dependency** on a sibling clone of
[NEAT-AI-core](https://github.com/stSoftwareAU/NEAT-AI-core).

Clone both repositories under the same parent directory so the path
dependency resolves:

```text
parent/
  NEAT-AI-core/      # clone of stSoftwareAU/NEAT-AI-core
  NEAT-AI-scorer/    # this repository
```

The `neat-core` path dependency is **unpinned** and tracks head, so a
**breaking** neat-core change can reach scorer silently. CI guards against
this with the **neat-core breaking-bump gate** (`scripts/check-neat-core-version.sh`):
it fails when neat-core's breaking component (major for `>= 1.0`, minor for
pre-1.0) climbs above the version recorded in
[`neat-core.expected-version`](./neat-core.expected-version). When the gate
fails, update `rust_scorer` for the breaking change and bump that baseline
file in the same PR. See the README "neat-core breaking-bump gate" section
for the full rationale.

## Prerequisites

The local gate and CI expect the following tools on your `PATH`:

- **Rust** — `cargo`, `rustc`, `clippy`, `rustfmt`. The exact compiler is pinned in [`rust-toolchain.toml`](./rust-toolchain.toml) and auto-installed by `rustup`, so local and CI builds use the same version (see the "Pinned Rust toolchain" section in the README for the bump cadence).
- **shellcheck** — lints the bash helper scripts.
- **cargo-deny** — licence and dependency audit (`cargo install cargo-deny --locked`).
- **codespell** — spell check (`pip install --user codespell`), driven by [`scripts/spell-check.sh`](./scripts/spell-check.sh).
- **bats** *(optional)* — runs the shell helper tests under `tests/scripts`.
- **cargo-edit** *(optional)* — enables the **opt-in** dependency upgrade step in `./quality.sh` (run `./quality.sh --upgrade`).

## Local gate

Run the full local quality gate before every commit or pull request:

```bash
./quality.sh < /dev/null
```

`./quality.sh` mirrors CI and runs, in order:

1. **shellcheck** — bash syntax and lint across all `*.sh` scripts.
2. **Workflow validators** — the `scripts/check-*.sh` guards over `.github/workflows`.
3. **codespell** — via `scripts/spell-check.sh`.
4. **bats** — shell helper tests under `tests/scripts` (when `bats` is installed).
5. **cargo-deny** — licence and advisory checks.
6. **`cargo fmt --all`** — formatting (CI runs `fmt --check`).
7. **`cargo clippy`** — lint with `-D warnings` plus `filter_next` and `collapsible_if`.
8. **`cargo check`**, **`cargo build`**, **`cargo test`** — type checks, debug build, and the test suite.
9. **`cargo doc`** — rustdoc with `RUSTDOCFLAGS=-D warnings`.
10. **Release build** — `cargo build --workspace --release`.

The default gate is **read-only** against `Cargo.lock` / `Cargo.toml` — it
never bumps dependency versions in your working tree. To bump library
dependencies during the gate, opt in with `./quality.sh --upgrade` (or
`QUALITY_UPGRADE=1 ./quality.sh`); this requires **cargo-edit**. Routine,
quarantine-gated bumps go through [`./bump-deps.sh`](./bump-deps.sh) instead.

Keep re-running `./quality.sh < /dev/null` until it passes cleanly.

## Coding standards

- **Australian English** throughout code, comments, and documentation
  (e.g. *colour*, *behaviour*, *organisation*, *favour*, *centre*).
- Keep the **positional** CLI contract (`<creature.json> <data_dir>`) stable.
- Add tests that exercise real behaviour — call functions with test data and
  assert on results, exit codes, or side effects.
- When a domain term trips codespell, add it with a short justification to
  [`.codespellrc`](./.codespellrc) rather than silencing a whole file.

## Pull request workflow

1. Branch from `Develop`.
2. Make your change with accompanying tests.
3. Run `./quality.sh < /dev/null` until it passes.
4. Update [`CHANGELOG.md`](./CHANGELOG.md) under the `## [Unreleased]`
   section, and update the README or other docs if behaviour changes.
5. Open a pull request targeting `Develop`.

On each PR the **Version Increment** workflow
([`.github/workflows/version-increment.yml`](./.github/workflows/version-increment.yml))
automatically bumps the patch component of `rust_scorer`'s version in
`rust_scorer/Cargo.toml` once, if it has not already been bumped on the
branch. Because the version is bumped automatically, the `CHANGELOG.md` is
the human-readable record of *what* changed — please keep it current.

## Licence

By contributing, you agree that your contributions are licensed under the
[Apache-2.0](./LICENSE) licence that covers this project.
