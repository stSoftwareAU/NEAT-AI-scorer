# NEAT-AI-scorer

Native **MSE scorer** CLI for NEAT-AI creatures. Shared logic lives in **`neat-core`**, resolved from a **path dependency** on **[NEAT-AI-core](https://github.com/stSoftwareAU/NEAT-AI-core)** (see `rust_scorer/Cargo.toml`). GitHub Actions checks out `NEAT-AI-core` next to this repo so CI can resolve that path.

## Source

| Component | Provenance |
|-----------|----------------|
| `rust_scorer/` | **`training_bin_stream::for_each_read_chunk`** (pipelined on native, same API on wasm) plus **pending + head + compact** (`stream_score.rs`), fused MSE when `forwardOnly` is true; **`float_scan_bench`** uses the same reader for throughput experiments. |
| `neat-core` (crate) | **`../../NEAT-AI-core/neat-core`** relative to `rust_scorer/Cargo.toml` — clone **NEAT-AI-core** as a sibling of **NEAT-AI-scorer** (same parent directory). |
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

JSON includes **`forwardOnly`** (from the creature) and **`trainingReadBackend`**: on a native release build you should see **`pipelined_double_buffer`** when `forwardOnly` is `true` (fused scoring + `training_bin_stream`). If `forwardOnly` is `false`, you get **`record_iterator`** instead (no pipelining — much slower on large data).

## Local layout

Place **NEAT-AI-core** and **NEAT-AI-scorer** as **siblings** (e.g. `…/src/NEAT-AI-core` and `…/src/NEAT-AI-scorer`). The path in `rust_scorer/Cargo.toml` is `../../NEAT-AI-core/neat-core` so `cargo build` resolves `neat-core` from your local **NEAT-AI-core** tree. CI does the same via a second checkout (`../NEAT-AI-core`).

## Relationship to NEAT-AI

Scorer-specific Rust stays here; **`neat-core`** tracks **NEAT-AI-core**.

## Why MSE-only?

The CLI scores creatures with **mean squared error** only — there is no `--cost` flag
and no runtime dispatch across loss functions.

- **Fused fast path is MSE.** The forward-only path calls
  `neat_core::loss::mse_sum_batch_packed` directly so error accumulation stays
  inside the same SIMD-friendly pass that reads packed `[inputs..., targets...]`
  records. The non-fused recurrent path (`forwardOnly: false`) uses
  `cost::mse_mean_record` to match the TypeScript `MSE.calculate()` mean.
- **Scope matches today's callers.** NEAT-AI `Develop` invokes this binary with
  the fixed positional contract `<creature.json> <data_dir>` (see `AGENTS.md`)
  and never requests a non-MSE score. `GROWTH_COST` and the fitness formula in
  `scoring.rs` are defined against MSE.
- **`neat-core` still exposes the full set.** The sibling crate already ships
  fused batch variants for MAE, cross-entropy, MAPE, MSLE, and hinge
  (`neat_core::loss::{mae,cross_entropy,mape,msle,hinge}_sum_batch_packed`).
  Re-adding a `--cost` dispatch would be CLI wiring plus tests — no new math —
  but until a downstream caller needs it, keeping the surface area small wins
  on KISS grounds and preserves the stable positional CLI contract.

If a downstream caller ever needs non-MSE scoring at this boundary, the
existing fused batch-packed losses in `neat-core` are the drop-in entry points;
see the in-tree `rust_scorer/` experiment on
[`milestone/pure-rust-scorer-experiment`](https://github.com/stSoftwareAU/NEAT-AI/blob/milestone/pure-rust-scorer-experiment/rust_scorer/src/cost.rs)
for the six-way dispatch pattern.

## License

Apache-2.0 — see `LICENSE`.
