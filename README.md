# NEAT-AI-scorer

Native **MSE scorer** CLI for NEAT-AI creatures. Shared logic lives in **`neat-core`**, pulled from **[NEAT-AI-core](https://github.com/stSoftwareAU/NEAT-AI-core)** (see `rust_scorer/Cargo.toml`).

## Source

| Component | Provenance |
|-----------|----------------|
| `rust_scorer/` | NEAT-AI working tree (not on `origin/Develop` yet): chunked read path with **pending buffer + head + compact** (`stream_score.rs`), fused MSE when `forwardOnly` is true, and **`float_scan_bench`** (`--mode=double-buf` / `mmap` / `read-copy`) for I/O experiments. |
| `neat-core` (crate) | **NEAT-AI-core** repository, `Develop` branch, via Cargo `git` dependency — not vendored here. |
| `LICENSE`, `.gitleaks.toml` | `origin/Develop` of NEAT-AI |

## Build

```bash
./quality.sh
```

Or step-by-step (matches CI):

```bash
export RUSTFLAGS="-D warnings"
cargo deny check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::filter_next -D clippy::collapsible_if
cargo test --workspace --all-features
cargo build --release -p rust_scorer
```

Requires **shellcheck**, **cargo-deny** (`cargo install cargo-deny --locked`), and optionally **cargo-edit** for the upgrade step in `./quality.sh`.

Binaries: `rust_scorer`, `float_scan_bench` (see `rust_scorer/Cargo.toml`).

## CLI

Positional arguments only (same contract as in NEAT-AI):

```text
rust_scorer <creature.json> <training_data_dir>
```

## Local development against a NEAT-AI-core checkout

If you clone [NEAT-AI-core](https://github.com/stSoftwareAU/NEAT-AI-core) alongside this repo, you can point `neat-core` at your working tree by temporarily replacing the `git` dependency in `rust_scorer/Cargo.toml` with `neat-core = { path = "../NEAT-AI-core/neat-core" }` (sibling layout). Prefer committing the `git` dependency so CI stays self-contained.

## Relationship to NEAT-AI

Scorer-specific Rust stays here; **`neat-core`** tracks **NEAT-AI-core**.

## License

Apache-2.0 — see `LICENSE`.
