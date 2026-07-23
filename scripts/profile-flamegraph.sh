#!/bin/bash
# scripts/profile-flamegraph.sh — Issue #37
#
# Capture a flamegraph for the fused scoring hot paths so optimisation work
# can be prioritised by real evidence rather than guesswork. The script wraps
# the portable pipeline:
#
#     sample <pid> → inferno-collapse-sample → inferno-flamegraph
#
# which works on macOS without `sudo`/SIP-disable (unlike `cargo flamegraph`,
# which drives `dtrace`). On Linux the same SVG output can be produced with
# `cargo flamegraph` (perf+inferno) — see `docs/performance-baseline.md`.
#
# Usage:
#   ./scripts/profile-flamegraph.sh [single_bytes] [multi_bytes] [multi_creatures]
#
#   single_bytes     .bin corpus size for single-creature run  (default: 2 GiB)
#   multi_bytes      .bin corpus size for multi-creature run   (default: 500 MB)
#   multi_creatures  Number of creatures for directory mode    (default: 50)
#
# Output:
#   docs/evidence/single-creature.svg
#   docs/evidence/multi-creature.svg
#
# Tunables (env):
#   PROFILE_SAMPLE_SECONDS  max time `sample` will wait for the child to exit
#                           (default 60; sample finalises once the child exits).
#   PROFILE_NUM_INPUTS      inputs per record   (default 8)
#   PROFILE_NUM_OUTPUTS     outputs per record  (default 2)
#   PROFILE_HIDDEN          hidden neurons      (default 8)
#   PROFILE_PROD_CREATURE   path to a LOCAL production network.json (Issue #296).
#                           When set, the real creature is used instead of the
#                           synthetic MLP; PROFILE_NUM_INPUTS / PROFILE_NUM_OUTPUTS
#                           are derived from it and the SVGs are written with a
#                           `-prod` suffix so the synthetic captures are not
#                           overwritten. This public repo ships no production
#                           creature and fetches nothing (Issue #448) — supply
#                           your own local file.
#
# Prerequisites:
#   cargo install inferno        # collapsed-stack → SVG
#   # macOS: Xcode command-line tools provide /usr/bin/sample.
#   # Linux: `cargo install flamegraph` then use `cargo flamegraph` instead.
set -euo pipefail

if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi

cd "$(dirname "$0")/.."
REPO_ROOT="$(pwd)"

SINGLE_BYTES="${1:-2147483648}"          # 2 GiB
MULTI_BYTES="${2:-524288000}"            # 500 MB
N_CREATURES="${3:-50}"
SAMPLE_SECONDS="${PROFILE_SAMPLE_SECONDS:-60}"
NUM_INPUTS="${PROFILE_NUM_INPUTS:-8}"
NUM_OUTPUTS="${PROFILE_NUM_OUTPUTS:-2}"
HIDDEN="${PROFILE_HIDDEN:-8}"

# Issue #296: production creature mode. When PROFILE_PROD_CREATURE points at the
# GRQ-cluster network.json, derive the input/output width from it and tag the
# output SVGs with `-prod` so the synthetic captures are preserved.
PROD_CREATURE="${PROFILE_PROD_CREATURE:-}"
SVG_SUFFIX=""
if [ -n "$PROD_CREATURE" ]; then
  if [ ! -s "$PROD_CREATURE" ]; then
    echo "❌ PROFILE_PROD_CREATURE=$PROD_CREATURE is missing or empty" >&2
    echo "   supply a LOCAL production network.json — this repo fetches nothing (Issue #448)" >&2
    exit 1
  fi
  # Derive dims from the real creature so the corpus matches it.
  read -r NUM_INPUTS NUM_OUTPUTS < <(
    python3 -c "import json,sys; d=json.load(open(sys.argv[1])); print(d['input'], d['output'])" \
      "$PROD_CREATURE"
  )
  SVG_SUFFIX="-prod"
  echo "🧬 Production creature mode: $PROD_CREATURE (inputs=$NUM_INPUTS outputs=$NUM_OUTPUTS)"
fi

for tool in sample inferno-collapse-sample inferno-flamegraph python3; do
  if ! command -v "$tool" > /dev/null 2>&1; then
    echo "❌ missing tool: $tool" >&2
    if [ "$tool" = "sample" ]; then
      echo "   install Xcode command-line tools (ships /usr/bin/sample)" >&2
    elif [ "$tool" = "python3" ]; then
      echo "   python3 is required to generate the synthetic training corpus" >&2
    else
      echo "   cargo install inferno" >&2
    fi
    exit 1
  fi
done

echo "🏗  Building rust_scorer with the profiling profile"
cargo build --profile profiling -p rust_scorer --bin rust_scorer

BIN="$REPO_ROOT/target/profiling/rust_scorer"
if [ ! -x "$BIN" ]; then
  echo "❌ built binary not found at $BIN" >&2
  exit 1
fi

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT
SINGLE_DATA="$TMPDIR/data-single"
MULTI_DATA="$TMPDIR/data-multi"
CREATURES_DIR="$TMPDIR/creatures"
mkdir -p "$SINGLE_DATA" "$MULTI_DATA" "$CREATURES_DIR"

mkdir -p "$REPO_ROOT/docs/evidence"
RAW_DIR="$REPO_ROOT/target/profiling-evidence"
mkdir -p "$RAW_DIR"

generate_fixture () {
  local data_dir="$1"
  local total_bytes="$2"
  local copy_to_creatures="$3"

  python3 - "$data_dir" "$CREATURES_DIR" "$total_bytes" "$N_CREATURES" \
              "$NUM_INPUTS" "$NUM_OUTPUTS" "$HIDDEN" "$TMPDIR" "$copy_to_creatures" \
              "$PROD_CREATURE" <<'PY'
import array, json, os, math, sys
(data_dir, creatures_dir, total_bytes, n_creatures, num_inputs, num_outputs,
 hidden, tmp_root, copy_creatures, prod_creature) = sys.argv[1:11]
total_bytes = int(total_bytes)
n_creatures = int(n_creatures)
num_inputs = int(num_inputs)
num_outputs = int(num_outputs)
hidden = int(hidden)

values_per_record = num_inputs + num_outputs
record_bytes = values_per_record * 4
n_records = max(1, total_bytes // record_bytes)

def make_creature():
    # Issue #296: use the real GRQ-cluster creature when provided.
    if prod_creature:
        with open(prod_creature) as f:
            return json.load(f)
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
single_path = os.path.join(tmp_root, "creature.json")
with open(single_path, "w") as f:
    json.dump(creature, f)

if copy_creatures == "1":
    for n in range(n_creatures):
        with open(os.path.join(creatures_dir, f"creature-{n:02d}.json"), "w") as f:
            json.dump(creature, f)

# Build a TILE of realistic-looking records once, then emit it repeatedly to
# reach the target corpus size. Content variation matters less for flamegraph
# profiling than numerical accuracy — we only need non-degenerate f32s that
# make the activation path do real arithmetic.
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
    written = 0
    remaining = n_records
    while remaining > 0:
        take = min(remaining, TILE_RECORDS)
        f.write(tile_bytes[: take * record_bytes])
        written += take * record_bytes
        remaining -= take
print(f"[fixture] {written} bytes / {n_records} records at {path}")
PY
}

echo "🧪 Generating single-creature fixture (${SINGLE_BYTES} bytes)"
generate_fixture "$SINGLE_DATA" "$SINGLE_BYTES" 1

echo "🧪 Generating multi-creature fixture (${MULTI_BYTES} bytes)"
generate_fixture "$MULTI_DATA" "$MULTI_BYTES" 0

SINGLE_CREATURE="$TMPDIR/creature.json"
if [ ! -s "$SINGLE_CREATURE" ]; then
  echo "❌ failed to generate single creature JSON at $SINGLE_CREATURE" >&2
  exit 1
fi

profile_scenario () {
  local label="$1"
  local svg_path="$2"
  shift 2
  local raw="$RAW_DIR/${label}.sample"
  local folded="$RAW_DIR/${label}.folded"

  echo "🔬 Profiling ${label} scenario"
  "$@" > /dev/null &
  local scorer_pid=$!

  echo "   sampling pid=$scorer_pid (max ${SAMPLE_SECONDS}s; sample finalises when child exits)"
  sample "$scorer_pid" "$SAMPLE_SECONDS" -file "$raw" > /dev/null
  wait "$scorer_pid" 2>/dev/null || true

  if [ ! -s "$raw" ]; then
    echo "❌ sample produced no output at $raw" >&2
    exit 1
  fi

  echo "   collapsing stacks → $folded"
  inferno-collapse-sample "$raw" > "$folded"

  echo "   rendering SVG → $svg_path"
  inferno-flamegraph --title "rust_scorer — ${label}" \
    --subtitle "sample-based flamegraph (Issue #37)" \
    "$folded" > "$svg_path"
}

profile_scenario "single-creature${SVG_SUFFIX}" \
  "$REPO_ROOT/docs/evidence/single-creature${SVG_SUFFIX}.svg" \
  "$BIN" "$SINGLE_CREATURE" "$SINGLE_DATA"

profile_scenario "multi-creature${SVG_SUFFIX}" \
  "$REPO_ROOT/docs/evidence/multi-creature${SVG_SUFFIX}.svg" \
  "$BIN" "$CREATURES_DIR" "$MULTI_DATA"

echo
echo "✅ Flamegraphs written:"
echo "   docs/evidence/single-creature${SVG_SUFFIX}.svg"
echo "   docs/evidence/multi-creature${SVG_SUFFIX}.svg"
echo "Raw sample output retained under target/profiling-evidence/ (gitignored)."
