#!/bin/bash
# scripts/bench-shallow-gpu.sh — Issue #467
#
# Wall-clock A/B of directory-mode scoring under `--gpu off` / `--gpu on` /
# `--gpu auto` for a **shallow** creature: thousands of inputs but only a
# handful of non-input neurons (the Enceladus shape — 2461 inputs, 19 hidden,
# 1 output). Inputs count towards `num_neurons`, so such creatures still route
# to the `forward_mse_scratch` kernel, yet they behave nothing like the deep
# production shape Issue #317 measured.
#
# The corpus is synthetic and generated locally at the caller's record width, so
# the run is fully autonomous — this public repo ships no creature and fetches
# nothing (Issue #448). Point BENCH_SHALLOW_CREATURE at a local creature JSON;
# with it unset the harness skips cleanly.
#
# Usage:
#   BENCH_SHALLOW_CREATURE=/path/to/Enceladus.json ./scripts/bench-shallow-gpu.sh
#   BENCH_SHALLOW_CREATURE=/a/Enceladus.json,/a/Enceladus-Terminal.json \
#     BENCH_SHALLOW_N=63 ./scripts/bench-shallow-gpu.sh
#
# Environment:
#   BENCH_SHALLOW_CREATURE     comma-separated local creature JSON paths (required)
#   BENCH_SHALLOW_N            creatures staged into the pool (default 50)
#   BENCH_SHALLOW_BINS         .bin shards in the synthetic corpus (default 4)
#   BENCH_SHALLOW_RECORDS      total records across all shards (default 37000)
#   BENCH_SHALLOW_REPS         timed repetitions per mode (default 5, median reported)
#   BENCH_SHALLOW_MODES        modes to time (default "off on auto")
#   BENCH_SHALLOW_FIXTURE_DIR  where the fixture is materialised
#                              (default $TMPDIR/neat-shallow-gpu-bench)
#   BENCH_SHALLOW_SCORER       scorer binary (default target/release/rust_scorer,
#                              built with `cargo build --release` when absent)
#   BENCH_SHALLOW_KEEP         set to 1 to keep the fixture after the run
#
# Requires `python3` (fixture generation + median arithmetic).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

CREATURE_SPEC="${BENCH_SHALLOW_CREATURE:-}"
POOL_N="${BENCH_SHALLOW_N:-50}"
NUM_BINS="${BENCH_SHALLOW_BINS:-4}"
NUM_RECORDS="${BENCH_SHALLOW_RECORDS:-37000}"
REPS="${BENCH_SHALLOW_REPS:-5}"
MODES="${BENCH_SHALLOW_MODES:-off on auto}"
FIXTURE_DIR="${BENCH_SHALLOW_FIXTURE_DIR:-${TMPDIR:-/tmp}/neat-shallow-gpu-bench}"

if [ -z "${CREATURE_SPEC}" ]; then
  echo "⏭️  BENCH_SHALLOW_CREATURE is unset — nothing to benchmark, skipping."
  echo "   This repo ships no creature and fetches none (Issue #448). Supply a"
  echo "   local shallow creature JSON, e.g.:"
  echo "     BENCH_SHALLOW_CREATURE=/path/to/Enceladus.json $0"
  exit 0
fi

if ! command -v python3 > /dev/null 2>&1; then
  echo "❌ python3 is required to generate the synthetic corpus" >&2
  exit 1
fi

# Split the comma-separated creature list (bash 3.2 compatible).
# Parameter expansion only — never assign IFS globally, which would change word
# splitting for every later unquoted expansion in this script.
CREATURES=()
REMAINING="$CREATURE_SPEC"
while [ -n "$REMAINING" ]; do
  case "$REMAINING" in
    *,*)
      p="${REMAINING%%,*}"
      REMAINING="${REMAINING#*,}"
      ;;
    *)
      p="$REMAINING"
      REMAINING=''
      ;;
  esac
  [ -n "$p" ] && CREATURES+=("$p")
done

if [ "${#CREATURES[@]}" -eq 0 ]; then
  echo "❌ BENCH_SHALLOW_CREATURE contained no usable path: ${CREATURE_SPEC}" >&2
  exit 1
fi

for c in "${CREATURES[@]}"; do
  if [ ! -f "$c" ]; then
    echo "❌ creature JSON not found: $c" >&2
    exit 1
  fi
done

SCORER="${BENCH_SHALLOW_SCORER:-${REPO_ROOT}/target/release/rust_scorer}"
if [ ! -x "$SCORER" ]; then
  echo "🛠  Building release scorer (missing: $SCORER)"
  if [ -f "$HOME/.cargo/env" ]; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
  fi
  (cd "$REPO_ROOT" && cargo build -p rust_scorer --release)
fi
if [ ! -x "$SCORER" ]; then
  echo "❌ scorer binary is not executable: $SCORER" >&2
  exit 1
fi

CREATURES_DIR="${FIXTURE_DIR}/creatures"
DATA_DIR="${FIXTURE_DIR}/data"
rm -rf "$FIXTURE_DIR"
mkdir -p "$CREATURES_DIR" "$DATA_DIR"

echo "🧪 Staging fixture in ${FIXTURE_DIR}"
echo "   creatures=${POOL_N}  bins=${NUM_BINS}  records=${NUM_RECORDS}  reps=${REPS}"

# Stage the pool and generate the corpus. Fails loud on a shape mismatch: a
# directory pool must share one (input, output) width or the scorer would be
# benchmarked on an invalid set.
python3 - "$CREATURES_DIR" "$DATA_DIR" "$POOL_N" "$NUM_BINS" "$NUM_RECORDS" \
  "${CREATURES[@]}" << 'PY'
import array, json, math, os, shutil, sys

creatures_dir, data_dir, pool_n, num_bins, num_records = sys.argv[1:6]
sources = sys.argv[6:]
pool_n, num_bins, num_records = int(pool_n), int(num_bins), int(num_records)

shape = None
for src in sources:
    with open(src) as fh:
        c = json.load(fh)
    this = (int(c["input"]), int(c["output"]))
    if shape is None:
        shape = this
    elif this != shape:
        sys.exit(
            f"creature shape mismatch: {src} is {this}, expected {shape} — "
            "a directory pool must share one input/output width"
        )
num_inputs, num_outputs = shape

# Round-robin the sources across the pool so each contributes evenly.
for n in range(pool_n):
    src = sources[n % len(sources)]
    stem = os.path.splitext(os.path.basename(src))[0]
    shutil.copyfile(src, os.path.join(creatures_dir, f"{stem}-{n:03d}.json"))

values_per_record = num_inputs + num_outputs
record_bytes = values_per_record * 4
per_bin = max(1, num_records // num_bins)

# Tile one deterministic block of records to fill each shard cheaply. Values are
# record-varying so activations stay out of a saturated regime.
tile_records = min(per_bin, 512)
tile = array.array("f", [0.0] * (tile_records * values_per_record))
idx = 0
for r in range(tile_records):
    for k in range(values_per_record):
        tile[idx] = math.sin((r * 31 + k) * 1.0e-3)
        idx += 1
tile_bytes = tile.tobytes()

total = 0
for b in range(num_bins):
    path = os.path.join(data_dir, f"{b}.bin")
    with open(path, "wb") as fh:
        remaining = per_bin
        while remaining > 0:
            take = min(remaining, tile_records)
            fh.write(tile_bytes[: take * record_bytes])
            remaining -= take
    total += per_bin
print(
    f"[fixture] {pool_n} creatures ({num_inputs} inputs, {num_outputs} outputs), "
    f"{total} records over {num_bins} bins "
    f"({total * record_bytes} bytes, {record_bytes} B/record)"
)
PY

RESULT_LINES=""
CPU_MEDIAN=""

# `time` writes to the enclosing stderr; the command's own streams are captured
# separately so only the elapsed seconds reach `elapsed`.
TIMEFORMAT='%R'

for mode in $MODES; do
  echo
  echo "⏱  --gpu ${mode}"
  samples=()
  backend=""
  for rep in $(seq 1 "$REPS"); do
    out="${FIXTURE_DIR}/out-${mode}.json"
    err="${FIXTURE_DIR}/err-${mode}.txt"
    rc_file="${FIXTURE_DIR}/rc-${mode}.txt"
    elapsed="$( { time "$SCORER" --gpu "$mode" "$CREATURES_DIR" "$DATA_DIR" \
      > "$out" 2> "$err"; echo "$?" > "$rc_file"; } 2>&1 )"
    rc="$(cat "$rc_file")"
    if [ "$rc" -ne 0 ]; then
      echo "❌ scorer failed under --gpu ${mode} (exit ${rc})" >&2
      sed -n '1,20p' "$err" >&2
      exit 1
    fi
    backend="$(sed -n 's/.*"gpuBackend": *"\([^"]*\)".*/\1/p' "$out" | head -n1)"
    samples+=("$elapsed")
    echo "   rep ${rep}: ${elapsed}s (${backend})"
  done

  median="$(python3 -c '
import statistics, sys
print(f"{statistics.median(float(x) for x in sys.argv[1:]):.2f}")' \
    ${samples[@]+"${samples[@]}"})"
  echo "   median: ${median}s"

  if [ "$mode" = "off" ]; then
    CPU_MEDIAN="$median"
    RESULT_LINES="${RESULT_LINES}| \`--gpu off\` | ${median} | ${backend} | CPU floor |"$'\n'
  else
    delta="n/a"
    if [ -n "$CPU_MEDIAN" ]; then
      # A CPU median of 0.00s means the run finished below the timer's
      # resolution, so a percentage is undefined — say so rather than dividing
      # by zero (which would abort the whole benchmark under `set -e`).
      delta="$(python3 -c '
import sys
cpu, other = float(sys.argv[1]), float(sys.argv[2])
if cpu <= 0.0:
    print("n/a (CPU median 0.00s — below timer resolution)")
else:
    print(f"{(cpu - other) / cpu * 100.0:+.1f}% vs CPU")' "$CPU_MEDIAN" "$median")"
      echo "   gpu ${mode} vs gpu off: ${delta}"
    fi
    RESULT_LINES="${RESULT_LINES}| \`--gpu ${mode}\` | ${median} | ${backend} | ${delta} |"$'\n'
  fi
done

echo
echo "📊 Median wall-clock (N=${POOL_N}, ${NUM_RECORDS} records, ${REPS} reps)"
echo
echo "| Mode | Wall (s) | gpuBackend | Delta |"
echo "|---|---|---|---|"
printf '%s' "$RESULT_LINES"

if [ "${BENCH_SHALLOW_KEEP:-0}" != "1" ]; then
  rm -rf "$FIXTURE_DIR"
fi
