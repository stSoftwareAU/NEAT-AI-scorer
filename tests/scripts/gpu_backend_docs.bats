#!/usr/bin/env bats
# Tests for scripts/check-gpu-backend-docs.sh — Issue #507.
#
# The validator keeps the README "Output" section's description of the
# `gpuBackend` JSON field aligned with the labels shipped in
# `GpuBackendLabel::as_str` (rust_scorer/src/gpu/mod.rs). Synthetic fixtures in
# a temp directory exercise the pass/fail behaviour, so the real README is
# never mutated by the tests.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-gpu-backend-docs.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  REPO_ROOT="${BATS_TEST_DIRNAME}/../.."
  export REPO_ROOT

  TMP_DIR="$(mktemp -d)"
  export TMP_DIR

  write_source "$TMP_DIR/mod.rs"
  write_readme "$TMP_DIR/README.md"
}

teardown() {
  rm -rf "$TMP_DIR"
}

run_check() {
  run "$SCRIPT_UNDER_TEST" \
    --readme "$TMP_DIR/README.md" \
    --source "$TMP_DIR/mod.rs"
}

# Minimal stand-in for GpuBackendLabel::as_str — the source of truth for the
# serialised labels the README must name.
write_source() {
  cat >"$1" <<'EOF'
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Metal => "metal",
            Self::Vulkan => "vulkan",
            Self::Dx12 => "dx12",
            Self::Gl => "gl",
            Self::CpuFallback => "cpu-fallback",
        }
    }
EOF
}

# README fixture whose Output section describes the shipped semantics.
write_readme() {
  cat >"$1" <<'EOF'
# Title

### Output

The **`gpuBackend`** field reports the backend that **actually ran** the
scoring kernel — `"metal"`, `"vulkan"`, `"dx12"` or `"gl"` when a GPU hosted
the run, and `"cpu-fallback"` when the CPU pipeline ran (see
[GPU mode](#gpu-mode-issues-80--83) above).

## Next section

Unrelated prose.
EOF
}

@test "passes when the Output section documents the shipped semantics" {
  run_check
  [ "$status" -eq 0 ]
  [[ "$output" == *"agrees with the shipped GpuBackendLabel values"* ]]
}

@test "fails on the stale 'until GPU kernels land' wording" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Title

### Output

The **`gpuBackend`** field reports which `wgpu` backend the scorer would run on
(`"cpu-fallback"` until GPU kernels land; see
[GPU mode](#gpu-mode-issues-80--83) above).

## Next section
EOF
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"until GPU kernels land"* ]]
}

@test "fails when the Output section omits a shipped backend label" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Title

### Output

The **`gpuBackend`** field reports the backend that **actually ran** the
scoring kernel — `"metal"`, `"vulkan"` or `"dx12"`, and `"cpu-fallback"` when
the CPU pipeline ran (see [GPU mode](#gpu-mode-issues-80--83) above).

## Next section
EOF
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *'does not name the "gl" gpuBackend label'* ]]
}

@test "fails when the Output section drops the runtime semantics" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Title

### Output

The **`gpuBackend`** field holds one of `"metal"`, `"vulkan"`, `"dx12"`,
`"gl"` or `"cpu-fallback"` (see [GPU mode](#gpu-mode-issues-80--83) above).

## Next section
EOF
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"actually ran"* ]]
}

@test "fails when the cross-link to the GPU mode section is lost" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Title

### Output

The **`gpuBackend`** field reports the backend that **actually ran** the
scoring kernel — `"metal"`, `"vulkan"`, `"dx12"` or `"gl"`, and
`"cpu-fallback"` when the CPU pipeline ran.

## Next section
EOF
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"cross-link"* ]]
}

@test "a new backend label added to the source fails until the README names it" {
  cat >>"$TMP_DIR/mod.rs" <<'EOF'
            Self::WebGpu => "webgpu",
EOF
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *'does not name the "webgpu" gpuBackend label'* ]]
}

@test "fails loud when the Output section is missing" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Title

## Something else

No output section here.
EOF
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"could not find the '### Output' section"* ]]
}

@test "fails loud when no labels can be read from the source" {
  echo "// no label arms here" >"$TMP_DIR/mod.rs"
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"could not read any GpuBackendLabel::as_str labels"* ]]
}

@test "fails loud when a file is missing" {
  rm -f "$TMP_DIR/README.md"
  run_check
  [ "$status" -eq 1 ]
  [[ "$output" == *"file not found"* ]]
}

@test "rejects unknown arguments with a usage error" {
  run "$SCRIPT_UNDER_TEST" --bogus
  [ "$status" -eq 2 ]
  [[ "$output" == *"Usage:"* ]]
}

@test "the real repository README satisfies the gpuBackend check" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
}
