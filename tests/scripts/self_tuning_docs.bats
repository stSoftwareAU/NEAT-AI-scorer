#!/usr/bin/env bats
# Tests for scripts/check-self-tuning-docs.sh — Issue #550.
#
# The guard keeps docs/self-tuning.md, README.md and docs/performance-baseline.md
# aligned with the knob resolvers in rust_scorer/src/{host_resources,read_tuning}.rs.
# Synthetic fixtures in a temp directory exercise the pass/fail behaviour, so the
# real documents are never mutated by the tests.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-self-tuning-docs.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_DIR="$(mktemp -d)"
  export TMP_DIR
  mkdir -p "$TMP_DIR/src/gpu"

  write_host_source "$TMP_DIR/src/host_resources.rs"
  write_read_source "$TMP_DIR/src/read_tuning.rs"
  write_knob_source "$TMP_DIR/src/gpu/forward_mse_batched.rs"
  write_doc "$TMP_DIR/self-tuning.md"
  write_readme "$TMP_DIR/README.md"
  write_baseline "$TMP_DIR/performance-baseline.md"
}

teardown() {
  rm -rf "$TMP_DIR"
}

run_check() {
  run "$SCRIPT_UNDER_TEST" \
    --doc "$TMP_DIR/self-tuning.md" \
    --readme "$TMP_DIR/README.md" \
    --baseline "$TMP_DIR/performance-baseline.md" \
    --src "$TMP_DIR/src"
}

# Stand-in for rust_scorer/src/host_resources.rs: the worker ceiling, the
# RAM-derived scratch tiers and the share divisors the doc quotes.
write_host_source() {
  cat >"$1" <<'EOF'
pub(crate) const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
const NAMEPLATE_TOLERANCE_DIVISOR: u64 = 16;
const UNIFIED_RAM_SHARE_DIVISOR: u64 = 16;
const DISCRETE_BINDING_SHARE_DIVISOR: u64 = 4;

pub fn max_worker_count(host: &HostResources) -> usize {
    match host.physical_ram_bytes {
        Some(ram) if ram < 2 * GIB => 2,
        Some(ram) if ram < 4 * GIB => 4,
        Some(ram) if ram < 8 * GIB => 16,
        None => 64,
        Some(_) => 256,
    }
}

fn ram_derived_gpu_scratch_bytes(host: &HostResources) -> u64 {
    match host.physical_ram_bytes {
        Some(ram) if ram < 4 * GIB => 64 * MIB,
        Some(ram) if ram < 8 * GIB => 128 * MIB,
        Some(ram) if ram < 16 * GIB => 256 * MIB,
        Some(ram) if ram >= 64 * GIB => 1024 * MIB,
        Some(_) | None => 512 * MIB,
    }
}
EOF
}

# Stand-in for rust_scorer/src/read_tuning.rs.
write_read_source() {
  cat >"$1" <<'EOF'
const DEFAULT_READ_BYTES: usize = 2 * 1024 * 1024;
const LARGE_RECORD_BYTES_THRESHOLD: usize = 8000;
const LARGE_RECORD_DEFAULT_READ_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_READ_BYTES: usize = 64 * 1024 * 1024;
const AGGREGATE_READ_BUDGET_BYTES: usize = 64 * 1024 * 1024;
const LARGE_RAM_AGGREGATE_READ_BUDGET_BYTES: usize = 256 * 1024 * 1024;
const AGGREGATE_READ_RAM_SHARE_DIVISOR: u64 = 16;

pub(crate) fn aggregate_read_budget_bytes(host: &HostResources) -> usize {
    let tier = match host.physical_ram_bytes {
        Some(ram) if ram >= 64 * GIB => LARGE_RAM_AGGREGATE_READ_BUDGET_BYTES,
        _ => AGGREGATE_READ_BUDGET_BYTES,
    };
    tier
}

pub(crate) fn default_training_read_bytes_for_readers(host: &HostResources) -> usize {
    let ram_cap = match host.physical_ram_bytes {
        Some(ram) if ram < 4 * GIB => DEFAULT_READ_BYTES,
        Some(ram) if ram < 8 * GIB => 8 * 1024 * 1024,
        Some(ram) if ram < 16 * GIB => 16 * 1024 * 1024,
        None => desired,
        Some(_) => host_resources::max_read_bytes(host),
    };
    ram_cap
}

pub fn training_read_target_bytes_from_env(record_bytes: usize) -> usize {
    let env = std::env::var("NEAT_SCORER_READ_BYTES").ok();
    record_bytes
}
EOF
}

# A second source file that reads a knob, so the inventory is genuinely a sweep
# of the tree rather than of one file.
write_knob_source() {
  cat >"$1" <<'EOF'
pub(crate) fn scratch_budget_bytes_from_env() -> u64 {
    let env = std::env::var("NEAT_SCORER_GPU_SCRATCH_BYTES").ok();
    0
}
EOF
}

# A self-tuning reference that agrees with the fixtures above.
write_doc() {
  cat >"$1" <<'EOF'
# Self-tuning reference

The scorer self-tunes from detected hardware. The `NEAT_SCORER_*` variables are
an **emergency escape hatch**, not per-host configuration.

## Detection

A probe within **6.25 %** of a nameplate capacity snaps up to it.

## Knobs

### Worker ceiling

| Snapped RAM | `max_worker_count` |
|---|---|
| < 2 GiB | 2 |
| < 4 GiB | 4 |
| < 8 GiB | 16 |
| ≥ 8 GiB | 256 |
| unknown | 64 |

### Read-chunk RAM ceiling

| Snapped RAM | Ceiling |
|---|---|
| < 4 GiB | 2 MiB |
| < 8 GiB | 8 MiB |
| < 16 GiB | 16 MiB |
| ≥ 16 GiB | 64 MiB |
| unknown | record-size default |

### Aggregate read budget

Never more than RAM / 16.

| Snapped RAM | Budget |
|---|---|
| ≥ 64 GiB | 256 MiB |
| unknown | 64 MiB |

### Record-size tier

| Record width | Default chunk |
|---|---|
| < 8000 B | 2 MiB |
| ≥ 8000 B | 32 MiB |

Clamped to 64 MiB.

### GPU scratch budget

Unified adapters are bounded by RAM / 16, discrete cards by binding limit / 4.

| Snapped RAM | Budget |
|---|---|
| < 4 GiB | 64 MiB |
| < 8 GiB | 128 MiB |
| < 16 GiB | 256 MiB |
| ≥ 64 GiB | 1 GiB |
| unknown | 512 MiB |

## Emergency escape hatches

| Variable | Status |
|---|---|
| `NEAT_SCORER_READ_BYTES` | emergency only |
| `NEAT_SCORER_GPU_SCRATCH_BYTES` | emergency only |
EOF
}

write_readme() {
  cat >"$1" <<'EOF'
# Title

The knobs are an **emergency escape hatch**, not per-host configuration — see
[the self-tuning reference](docs/self-tuning.md).
EOF
}

write_baseline() {
  cat >"$1" <<'EOF'
# Performance baseline

Every knob below is an **emergency escape hatch**, not per-host configuration;
the shipped policy lives in [self-tuning.md](self-tuning.md).
EOF
}

@test "passes on documents aligned with the shipped constants" {
  run_check
  [ "$status" -eq 0 ]
  [[ "$output" == *"agree with"* ]]
}

@test "fails when a documented worker-ceiling tier drifts from the code" {
  sed -i.bak 's/| ≥ 8 GiB | 256 |/| ≥ 8 GiB | 128 |/' "$TMP_DIR/self-tuning.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"Worker ceiling"* ]]
  [[ "$output" == *"256"* ]]
}

@test "fails when the code gains a worker tier the doc does not have" {
  sed -i.bak 's/        None => 64,/        Some(ram) if ram < 12 * GIB => 32,\n        None => 64,/' \
    "$TMP_DIR/src/host_resources.rs"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"< 12 GiB"* ]]
}

@test "fails when a GPU scratch tier drifts from the code" {
  sed -i.bak 's/| ≥ 64 GiB | 1 GiB |/| ≥ 64 GiB | 2 GiB |/' "$TMP_DIR/self-tuning.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"GPU scratch budget"* ]]
}

@test "fails when the read-chunk clamp is bumped in code but not in the doc" {
  sed -i.bak 's/pub(crate) const MAX_READ_BYTES: usize = 64 \* 1024 \* 1024;/pub(crate) const MAX_READ_BYTES: usize = 128 * 1024 * 1024;/' \
    "$TMP_DIR/src/read_tuning.rs"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"128 MiB"* ]]
}

@test "fails when the record-size threshold drifts" {
  sed -i.bak 's/const LARGE_RECORD_BYTES_THRESHOLD: usize = 8000;/const LARGE_RECORD_BYTES_THRESHOLD: usize = 6000;/' \
    "$TMP_DIR/src/read_tuning.rs"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"6000 B"* ]]
}

@test "fails when the nameplate tolerance drifts from the divisor" {
  sed -i.bak 's/const NAMEPLATE_TOLERANCE_DIVISOR: u64 = 16;/const NAMEPLATE_TOLERANCE_DIVISOR: u64 = 8;/' \
    "$TMP_DIR/src/host_resources.rs"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"12.50 %"* ]]
}

@test "fails when a NEAT_SCORER_ knob in the sources has no doc entry" {
  # The negative control the guard exists for: a fabricated knob name that no
  # document mentions must fail, so a real new knob cannot ship undocumented.
  cat >"$TMP_DIR/src/fabricated.rs" <<'EOF'
pub fn fabricated_knob() -> Option<String> {
    std::env::var("NEAT_SCORER_FABRICATED_KNOB").ok()
}
EOF
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"NEAT_SCORER_FABRICATED_KNOB"* ]]
}

@test "fails when the knob inventory finds nothing at all" {
  # Guard self-check: an inventory that silently matches no knob would let the
  # coverage rule pass vacuously.
  rm -f "$TMP_DIR/src/read_tuning.rs" "$TMP_DIR/src/gpu/forward_mse_batched.rs"
  write_read_source "$TMP_DIR/src/read_tuning.rs"
  sed -i.bak 's/std::env::var("NEAT_SCORER_READ_BYTES")/no_env_read()/' "$TMP_DIR/src/read_tuning.rs"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"inventory is broken"* ]]
}

@test "fails when the README drops the emergency-only wording" {
  sed -i.bak 's/an \*\*emergency escape hatch\*\*, not per-host configuration/a tuning knob you are expected to set/' \
    "$TMP_DIR/README.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"emergency escape hatch"* ]]
}

@test "fails when the performance baseline drops the emergency-only wording" {
  sed -i.bak 's/an \*\*emergency escape hatch\*\*, not per-host configuration/a per-host tuning recipe/' \
    "$TMP_DIR/performance-baseline.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"performance-baseline.md"* ]]
}

@test "fails when the README loses its link to the reference doc" {
  sed -i.bak 's|(docs/self-tuning.md)|(docs/tuning.md)|' "$TMP_DIR/README.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"link to the self-tuning reference"* ]]
}

@test "fails when a documented section is removed entirely" {
  sed -i.bak '/^### GPU scratch budget$/,/^## Emergency escape hatches$/d' "$TMP_DIR/self-tuning.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"could not find"* ]]
}

@test "reports an error when a document is missing" {
  rm -f "$TMP_DIR/self-tuning.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -eq 2 ]
  [[ "$output" == *"Usage"* ]]
}

@test "the real repository documents satisfy the check" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
