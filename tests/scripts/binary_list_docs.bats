#!/usr/bin/env bats
# Tests for scripts/check-binary-list-docs.sh — Issue #509.
#
# The validator keeps the workspace binary list single-homed: the README
# "Binaries" section must name every `[[bin]]` target in
# `rust_scorer/Cargo.toml`, and CONTRIBUTING.md / AGENTS.md must cite that home
# instead of carrying their own (drift-prone) copies. Synthetic fixtures in a
# temp directory exercise the behaviour, so the real docs are never mutated.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-binary-list-docs.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_DIR="$(mktemp -d)"
  export TMP_DIR

  write_manifest "$TMP_DIR/Cargo.toml"
  write_readme "$TMP_DIR/README.md"
  write_contributing "$TMP_DIR/CONTRIBUTING.md"
  write_agents "$TMP_DIR/AGENTS.md"
}

teardown() {
  rm -rf "$TMP_DIR"
}

run_check() {
  run "$SCRIPT_UNDER_TEST" \
    --manifest "$TMP_DIR/Cargo.toml" \
    --readme "$TMP_DIR/README.md" \
    --contributing "$TMP_DIR/CONTRIBUTING.md" \
    --agents "$TMP_DIR/AGENTS.md"
}

# Stand-in for rust_scorer/Cargo.toml — the source of truth for the binaries.
write_manifest() {
  cat >"$1" <<'EOF'
[package]
name = "rust_scorer"

[[bin]]
name = "rust_scorer"
path = "src/main.rs"

[[bin]]
name = "float_scan_bench"
path = "src/bin/float_scan_bench.rs"

[[bin]]
name = "cost_scan_bench"
path = "src/bin/cost_scan_bench.rs"

[[bin]]
name = "gpu_pipeline_alloc_bench"
path = "src/bin/gpu_pipeline_alloc_bench.rs"
EOF
}

write_readme() {
  cat >"$1" <<'EOF'
# Title

### Binaries

Binaries: `rust_scorer`, `float_scan_bench`, `cost_scan_bench`,
`gpu_pipeline_alloc_bench` (see `rust_scorer/Cargo.toml`).

## CLI

Unrelated prose.
EOF
}

write_contributing() {
  cat >"$1" <<'EOF'
# Contributing

## Repository layout

The sole workspace member is **`rust_scorer`**. Its `[[bin]]` targets are owned
by [`rust_scorer/Cargo.toml`](./rust_scorer/Cargo.toml) and documented in the
README [Binaries](./README.md#binaries) section.
EOF
}

write_agents() {
  cat >"$1" <<'EOF'
# AGENTS.md

- Workspace member: **`rust_scorer`** — its `[[bin]]` targets are owned by
  **`rust_scorer/Cargo.toml`** and documented in the README
  [Binaries](./README.md#binaries) section.
EOF
}

@test "passes when the README lists every binary and the other docs cite it" {
  run_check
  [ "$status" -eq 0 ]
  [[ "$output" == *"single-homed"* ]]
}

@test "fails when the README omits a manifest binary" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Title

### Binaries

Binaries: `rust_scorer`, `float_scan_bench`, `cost_scan_bench` (see
`rust_scorer/Cargo.toml`).

## CLI
EOF
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"gpu_pipeline_alloc_bench"* ]]
}

@test "a new binary added to the manifest fails until the README names it" {
  cat >>"$TMP_DIR/Cargo.toml" <<'EOF'

[[bin]]
name = "shiny_new_bench"
path = "src/bin/shiny_new_bench.rs"
EOF
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"shiny_new_bench"* ]]
}

@test "fails when CONTRIBUTING.md restates the binary list" {
  cat >"$TMP_DIR/CONTRIBUTING.md" <<'EOF'
# Contributing

## Repository layout

The sole workspace member is **`rust_scorer`** (the `rust_scorer`,
`float_scan_bench`, and `cost_scan_bench` binaries). See
[Binaries](./README.md#binaries).
EOF
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"CONTRIBUTING.md"* ]]
  [[ "$output" == *"float_scan_bench"* ]]
}

@test "fails when AGENTS.md restates the binary list" {
  cat >"$TMP_DIR/AGENTS.md" <<'EOF'
# AGENTS.md

- Workspace member: **`rust_scorer`** (CLI + `float_scan_bench`). See
  [Binaries](./README.md#binaries).
EOF
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"AGENTS.md"* ]]
  [[ "$output" == *"float_scan_bench"* ]]
}

@test "fails when CONTRIBUTING.md drops its citation of the binary list home" {
  cat >"$TMP_DIR/CONTRIBUTING.md" <<'EOF'
# Contributing

## Repository layout

The sole workspace member is **`rust_scorer`**.
EOF
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"cite"* ]]
}

@test "fails when AGENTS.md drops its citation of the binary list home" {
  cat >"$TMP_DIR/AGENTS.md" <<'EOF'
# AGENTS.md

- Workspace member: **`rust_scorer`**.
EOF
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"cite"* ]]
}

@test "fails loud when the README Binaries section is missing" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Title

## CLI

No binaries section here.
EOF
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"could not find the '### Binaries' section"* ]]
}

@test "fails loud when the manifest declares no binaries" {
  echo "[package]" >"$TMP_DIR/Cargo.toml"
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"could not read any [[bin]] targets"* ]]
}

@test "fails loud when a file is missing" {
  rm -f "$TMP_DIR/AGENTS.md"
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"file not found"* ]]
}

@test "rejects unknown arguments with a usage error" {
  run "$SCRIPT_UNDER_TEST" --bogus
  [ "$status" -eq 2 ]
  [[ "$output" == *"Usage:"* ]]
}

@test "the real repository documents satisfy the binary-list check" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
}
