# AGENTS.md

- Workspace member: **`rust_scorer`** (CLI + `float_scan_bench`). It depends on **`neat-core`** from **[NEAT-AI-core](https://github.com/stSoftwareAU/NEAT-AI-core)** (`git` + `Develop` in `rust_scorer/Cargo.toml`).
- Keep the **positional** CLI contract (`<creature.json> <data_dir>`) stable.
- Local gate: run **`./quality.sh`** before commit/PR (shellcheck, optional `cargo upgrade`, **cargo-deny**, `fmt --check`, clippy, check, build, test, `doc` with `RUSTDOCFLAGS=-D warnings`, release build).
- CI (`.github/workflows/ci.yml`): `fmt`, **cargo-deny**, `clippy` (`filter_next` / `collapsible_if`), `check`, `build`, `test`, **rustdoc**, plus **shellcheck** on `*.sh`; **validation** (required files + `cargo metadata`); **codespell**; PR-only **security** (`cargo audit` + dependency review). Weekly **`upgrade-dependencies.yml`** opens a bump PR when `cargo upgrade` changes manifests.
- Extra `clippy::*` allows live in workspace `Cargo.toml` so `-D warnings` is viable on this imported code; align with Discovery pedantic rules gradually if you want stricter parity.
