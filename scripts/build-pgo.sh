#!/bin/bash
# scripts/build-pgo.sh — Issue #43
#
# Build `rust_scorer` with Profile-Guided Optimisation (PGO). The release
# profile already enables LTO and `codegen-units = 1`; PGO is the next
# compiler-level lever — a recorded profile of a real scoring run feeds back
# into `rustc` so hot loops get better inlining, branch prediction hints, and
# code layout.
#
# Pipeline (manual `-Cprofile-generate` / `-Cprofile-use` flow — no extra
# `cargo-pgo` install required):
#
#   1. Build instrumented binary with `RUSTFLAGS=-Cprofile-generate=<dir>`.
#   2. Run `rust_scorer` against a representative training-data fixture
#      (single-creature mode + directory mode) to gather `*.profraw` files.
#   3. Merge the raw profiles via `llvm-profdata merge`.
#   4. Re-build with `RUSTFLAGS=-Cprofile-use=<merged.profdata>`.
#
# Output: `target/pgo/rust_scorer` is the final PGO-optimised binary.
#
# Usage:
#   ./scripts/build-pgo.sh
#
# Tunables (env):
#   PGO_BYTES         training corpus size in bytes (default 100 MB)
#   PGO_NUM_INPUTS    inputs per record   (default 8)
#   PGO_NUM_OUTPUTS   outputs per record  (default 2)
#   PGO_HIDDEN        hidden neurons      (default 8)
#   PGO_CREATURES     creatures for directory-mode pass (default 10)
#   PGO_PROFDATA_DIR  where to drop *.profraw + merged.profdata
#                     (default: $REPO_ROOT/target/pgo-profiles)
#   PGO_FIXTURE_DIR   where to materialise the synthetic training fixture
#                     (default: $REPO_ROOT/target/pgo-fixture)
#   LLVM_PROFDATA     override the llvm-profdata binary (otherwise auto-discovered:
#                     `command -v llvm-profdata`, then via `rustc --print sysroot`)
#
# Prerequisites:
#   * `rustup component add llvm-tools` (provides `llvm-profdata`).
#   * `python3` (used to materialise a deterministic synthetic training corpus).
set -euo pipefail

if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi

cd "$(dirname "$0")/.."
REPO_ROOT="$(pwd)"

PGO_BYTES="${PGO_BYTES:-104857600}"
PGO_NUM_INPUTS="${PGO_NUM_INPUTS:-8}"
PGO_NUM_OUTPUTS="${PGO_NUM_OUTPUTS:-2}"
PGO_HIDDEN="${PGO_HIDDEN:-8}"
PGO_CREATURES="${PGO_CREATURES:-10}"
PROFDATA_DIR="${PGO_PROFDATA_DIR:-${REPO_ROOT}/target/pgo-profiles}"
FIXTURE_DIR="${PGO_FIXTURE_DIR:-${REPO_ROOT}/target/pgo-fixture}"

# Locate llvm-profdata. Prefer a caller-supplied override, then PATH, then the
# llvm-tools component installed under the active rustup toolchain. The
# rustup-shipped binary is not on PATH by default, so the sysroot fallback is
# the common case on a fresh workstation that has run
# `rustup component add llvm-tools`.
LLVM_PROFDATA="${LLVM_PROFDATA:-}"
if [ -z "${LLVM_PROFDATA}" ] && command -v llvm-profdata > /dev/null 2>&1; then
  LLVM_PROFDATA="$(command -v llvm-profdata)"
fi
if [ -z "${LLVM_PROFDATA}" ]; then
  if command -v rustc > /dev/null 2>&1; then
    SYSROOT="$(rustc --print sysroot)"
    candidate="$(find "${SYSROOT}/lib/rustlib" -type f -name 'llvm-profdata' 2>/dev/null | head -n1 || true)"
    if [ -n "${candidate}" ] && [ -x "${candidate}" ]; then
      LLVM_PROFDATA="${candidate}"
    fi
  fi
fi
if [ -z "${LLVM_PROFDATA}" ] || [ ! -x "${LLVM_PROFDATA}" ]; then
  echo "❌ llvm-profdata not found"
  echo "   install: rustup component add llvm-tools"
  exit 1
fi

if ! command -v python3 > /dev/null 2>&1; then
  echo "❌ python3 required to generate the PGO training fixture"
  exit 1
fi

mkdir -p "${PROFDATA_DIR}" "${FIXTURE_DIR}"
# Wipe any stale raw or merged profile data from a previous run; mixing
# different builds' profiles confuses `llvm-profdata merge`.
find "${PROFDATA_DIR}" -maxdepth 1 -type f \( -name '*.profraw' -o -name 'merged.profdata' \) -delete 2>/dev/null || true

DATA_DIR="${FIXTURE_DIR}/data"
CREATURES_DIR="${FIXTURE_DIR}/creatures"
SINGLE_CREATURE="${FIXTURE_DIR}/creature.json"
mkdir -p "${DATA_DIR}" "${CREATURES_DIR}"

echo "🧪 Generating PGO training fixture (${PGO_BYTES} bytes, ${PGO_CREATURES} creatures)"
python3 - "$DATA_DIR" "$CREATURES_DIR" "$SINGLE_CREATURE" "$PGO_BYTES" \
            "$PGO_CREATURES" "$PGO_NUM_INPUTS" "$PGO_NUM_OUTPUTS" "$PGO_HIDDEN" <<'PY'
import array, json, os, math, sys
(data_dir, creatures_dir, single_path, total_bytes, n_creatures,
 num_inputs, num_outputs, hidden) = sys.argv[1:9]
total_bytes = int(total_bytes)
n_creatures = int(n_creatures)
num_inputs = int(num_inputs)
num_outputs = int(num_outputs)
hidden = int(hidden)

values_per_record = num_inputs + num_outputs
record_bytes = values_per_record * 4
n_records = max(1, total_bytes // record_bytes)

def make_creature():
    neurons = []
    for h in range(hidden):
        neurons.append({"type": "hidden", "uuid": f"hidden-{h}",
                        "bias": 0.05, "squash": "TANH"})
    for o in range(num_outputs):
        neurons.append({"type": "output", "uuid": f"output-{o}",
                        "bias": 0.0, "squash": "IDENTITY"})
    synapses = []
    for i in range(num_inputs):
        for h in range(hidden):
            synapses.append({"fromUUID": f"input-{i}",
                             "toUUID": f"hidden-{h}",
                             "weight": 0.05 + 0.001 * (i * hidden + h)})
    for h in range(hidden):
        for o in range(num_outputs):
            synapses.append({"fromUUID": f"hidden-{h}",
                             "toUUID": f"output-{o}",
                             "weight": 0.1 + 0.001 * (h * num_outputs + o)})
    return {
        "input": num_inputs, "output": num_outputs, "forwardOnly": True,
        "semanticVersion": "4.0.0", "neurons": neurons, "synapses": synapses,
    }

creature = make_creature()
with open(single_path, "w") as f:
    json.dump(creature, f)
for n in range(n_creatures):
    with open(os.path.join(creatures_dir, f"creature-{n:02d}.json"), "w") as f:
        json.dump(creature, f)

# Tile a deterministic block of records to reach the target corpus size.
TILE_RECORDS = min(n_records, 1 << 14)
tile = array.array("f", [0.0] * (TILE_RECORDS * values_per_record))
idx = 0
for r in range(TILE_RECORDS):
    for k in range(values_per_record):
        tile[idx] = math.sin((r * 31 + k) * 1.0e-3)
        idx += 1
tile_bytes = tile.tobytes()
path = os.path.join(data_dir, "0.bin")
with open(path, "wb") as f:
    remaining = n_records
    while remaining > 0:
        take = min(remaining, TILE_RECORDS)
        f.write(tile_bytes[: take * record_bytes])
        remaining -= take
print(f"[fixture] {n_records} records at {path}")
PY

INSTRUMENTED_BIN="${REPO_ROOT}/target/pgo/rust_scorer"

echo "🛠  Phase 1/4: Building instrumented binary (-Cprofile-generate)"
RUSTFLAGS="-Cprofile-generate=${PROFDATA_DIR}" \
  cargo build --profile pgo -p rust_scorer --bin rust_scorer

if [ ! -x "${INSTRUMENTED_BIN}" ]; then
  echo "❌ instrumented binary not found at ${INSTRUMENTED_BIN}" >&2
  exit 1
fi

echo "📊 Phase 2/4: Gathering profile (single-creature scoring run)"
"${INSTRUMENTED_BIN}" "${SINGLE_CREATURE}" "${DATA_DIR}" > /dev/null

echo "📊 Phase 2/4: Gathering profile (directory-mode scoring run)"
"${INSTRUMENTED_BIN}" "${CREATURES_DIR}" "${DATA_DIR}" > /dev/null

echo "🔀 Phase 3/4: Merging *.profraw → merged.profdata"
"${LLVM_PROFDATA}" merge -o "${PROFDATA_DIR}/merged.profdata" "${PROFDATA_DIR}"

if [ ! -s "${PROFDATA_DIR}/merged.profdata" ]; then
  echo "❌ merged.profdata is empty — instrumentation produced no data" >&2
  exit 1
fi

echo "🏗  Phase 4/4: Building final PGO-optimised binary (-Cprofile-use)"
RUSTFLAGS="-Cprofile-use=${PROFDATA_DIR}/merged.profdata -Cllvm-args=-pgo-warn-missing-function" \
  cargo build --profile pgo -p rust_scorer --bin rust_scorer

echo
echo "✅ PGO binary ready: ${INSTRUMENTED_BIN}"
echo "   Merged profile:  ${PROFDATA_DIR}/merged.profdata"
