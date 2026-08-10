#!/bin/bash
# scripts/bench-knob-sweep.sh — Issue #545 (enabler for the #544 retune chain)
#
# Sweeps ONE host-tuned knob across a caller-supplied list of values on the
# production scoring path and reports the median wall-clock per value, in the
# same table shape as `scripts/bench-shallow-gpu.sh`. Every #544 sub-issue cites
# its before/after numbers from here instead of hand-rolling a measurement rig.
#
# The run opens with the scorer's own `--host-report` JSON, so a pasted sweep
# carries the host it was measured on (logical CPUs, RAM, resolved knobs).
#
# This public repo ships no production creature and no corpus, and fetches
# neither (Issue #448): point BENCH_SWEEP_CREATURE and BENCH_SWEEP_DATA at local
# inputs. With either unset the harness skips cleanly (exit 0); once they are
# supplied it is fail-loud — an unreadable input or a failed scoring run exits
# non-zero rather than reporting an empty sweep as success.
#
# Usage:
#   BENCH_SWEEP_CREATURE=/path/to/creatures_dir BENCH_SWEEP_DATA=/path/to/corpus \
#     BENCH_SWEEP_KNOB=NEAT_SCORER_READ_BYTES \
#     BENCH_SWEEP_VALUES=default,2097152,8388608,33554432 \
#     ./scripts/bench-knob-sweep.sh
#
# Environment:
#   BENCH_SWEEP_CREATURE  local creature JSON *or* creatures directory (required)
#   BENCH_SWEEP_DATA      local training-data directory of .bin files (required)
#   BENCH_SWEEP_KNOB      NEAT_SCORER_* env var to sweep
#                         (default NEAT_SCORER_READ_BYTES)
#   BENCH_SWEEP_VALUES    comma-separated values; the literal `default` runs
#                         with the knob unset (default "default" — a
#                         single-knob-neutral baseline run)
#   BENCH_SWEEP_REPS      timed repetitions per value (default 5, median reported)
#   BENCH_SWEEP_GPU       --gpu mode for every run (default auto, as production
#                         omits the flag)
#   BENCH_SWEEP_SCORER    scorer binary (default target/release/rust_scorer,
#                         built with `cargo build --release` when absent)
#
# Requires `python3` (median arithmetic).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

CREATURE="${BENCH_SWEEP_CREATURE:-}"
DATA_DIR="${BENCH_SWEEP_DATA:-}"
KNOB="${BENCH_SWEEP_KNOB:-NEAT_SCORER_READ_BYTES}"
VALUE_SPEC="${BENCH_SWEEP_VALUES:-default}"
REPS="${BENCH_SWEEP_REPS:-5}"
GPU_MODE="${BENCH_SWEEP_GPU:-auto}"

if [ -z "$CREATURE" ] || [ -z "$DATA_DIR" ]; then
  echo "⏭️  BENCH_SWEEP_CREATURE / BENCH_SWEEP_DATA are unset — nothing to sweep, skipping."
  echo "   This repo ships no creature or corpus and fetches none (Issue #448)."
  echo "   Supply local inputs, e.g.:"
  echo "     BENCH_SWEEP_CREATURE=/path/to/creatures BENCH_SWEEP_DATA=/path/to/corpus $0"
  exit 0
fi

if ! command -v python3 > /dev/null 2>&1; then
  echo "❌ python3 is required for the median arithmetic" >&2
  exit 1
fi

if [ ! -e "$CREATURE" ] || [ ! -r "$CREATURE" ]; then
  echo "❌ BENCH_SWEEP_CREATURE is not a readable file or directory: ${CREATURE}" >&2
  exit 1
fi
if [ ! -d "$DATA_DIR" ] || [ ! -r "$DATA_DIR" ]; then
  echo "❌ BENCH_SWEEP_DATA is not a readable directory: ${DATA_DIR}" >&2
  exit 1
fi

# Allowlist the knob name: it is injected into the scorer's environment, so only
# the scorer's own tuning namespace is accepted.
case "$KNOB" in
  NEAT_SCORER_*)
    if ! printf '%s' "$KNOB" | grep -Eq '^NEAT_SCORER_[A-Z0-9_]+$'; then
      echo "❌ BENCH_SWEEP_KNOB must match NEAT_SCORER_[A-Z0-9_]+: ${KNOB}" >&2
      exit 1
    fi
    ;;
  *)
    echo "❌ BENCH_SWEEP_KNOB must be a NEAT_SCORER_* tuning variable: ${KNOB}" >&2
    exit 1
    ;;
esac

if ! printf '%s' "$REPS" | grep -Eq '^[1-9][0-9]*$'; then
  echo "❌ BENCH_SWEEP_REPS must be a positive integer: ${REPS}" >&2
  exit 1
fi

case "$GPU_MODE" in
  auto | on | off) ;;
  *)
    echo "❌ BENCH_SWEEP_GPU must be auto, on or off: ${GPU_MODE}" >&2
    exit 1
    ;;
esac

# Split the comma-separated value list (bash 3.2 compatible; parameter expansion
# only — never assign IFS globally).
VALUES=()
REMAINING="$VALUE_SPEC"
while [ -n "$REMAINING" ]; do
  case "$REMAINING" in
    *,*)
      v="${REMAINING%%,*}"
      REMAINING="${REMAINING#*,}"
      ;;
    *)
      v="$REMAINING"
      REMAINING=''
      ;;
  esac
  [ -n "$v" ] && VALUES+=("$v")
done

if [ "${#VALUES[@]}" -eq 0 ]; then
  echo "❌ BENCH_SWEEP_VALUES contained no usable value: ${VALUE_SPEC}" >&2
  exit 1
fi

for v in "${VALUES[@]}"; do
  if [ "$v" != "default" ] && ! printf '%s' "$v" | grep -Eq '^[0-9]+$'; then
    echo "❌ BENCH_SWEEP_VALUES entries must be 'default' or a non-negative integer: ${v}" >&2
    exit 1
  fi
done

SCORER="${BENCH_SWEEP_SCORER:-${REPO_ROOT}/target/release/rust_scorer}"
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

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/neat-knob-sweep.XXXXXX")"
cleanup() { rm -rf "$WORK_DIR"; }
trap cleanup EXIT

echo "🖥  Host knob report"
if ! "$SCORER" --host-report > "${WORK_DIR}/host-report.json" 2> "${WORK_DIR}/host-report.err"; then
  echo "❌ ${SCORER} --host-report failed — the harness cannot record the host it measured" >&2
  sed -n '1,20p' "${WORK_DIR}/host-report.err" >&2
  exit 1
fi
cat "${WORK_DIR}/host-report.json"

echo
echo "🔁 Sweeping ${KNOB} over: ${VALUES[*]}"
echo "   creature=${CREATURE}  data=${DATA_DIR}  reps=${REPS}  --gpu ${GPU_MODE}"

RESULT_LINES=""
BASELINE_MEDIAN=""

# `time` writes to the enclosing stderr; the command's own streams are captured
# separately so only the elapsed seconds reach `elapsed`.
TIMEFORMAT='%R'

for value in "${VALUES[@]}"; do
  echo
  echo "⏱  ${KNOB}=${value}"
  samples=()
  backend=""
  for rep in $(seq 1 "$REPS"); do
    out="${WORK_DIR}/out-${value}.json"
    err="${WORK_DIR}/err-${value}.txt"
    rc_file="${WORK_DIR}/rc-${value}.txt"
    if [ "$value" = "default" ]; then
      elapsed="$( { time env -u "$KNOB" "$SCORER" --gpu "$GPU_MODE" "$CREATURE" "$DATA_DIR" \
        > "$out" 2> "$err"; echo "$?" > "$rc_file"; } 2>&1 )"
    else
      elapsed="$( { time env "${KNOB}=${value}" "$SCORER" --gpu "$GPU_MODE" "$CREATURE" "$DATA_DIR" \
        > "$out" 2> "$err"; echo "$?" > "$rc_file"; } 2>&1 )"
    fi
    rc="$(cat "$rc_file")"
    if [ "$rc" -ne 0 ]; then
      echo "❌ scorer failed at ${KNOB}=${value} (exit ${rc})" >&2
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

  if [ -z "$BASELINE_MEDIAN" ]; then
    BASELINE_MEDIAN="$median"
    RESULT_LINES="${RESULT_LINES}| \`${value}\` | ${median} | ${backend} | baseline |"$'\n'
  else
    # A baseline median of 0.00s means the run finished below the timer's
    # resolution, so a percentage is undefined — say so rather than dividing by
    # zero (which would abort the sweep under `set -e`).
    delta="$(python3 -c '
import sys
base, other = float(sys.argv[1]), float(sys.argv[2])
if base <= 0.0:
    print("n/a (baseline median 0.00s — below timer resolution)")
else:
    print(f"{(base - other) / base * 100.0:+.1f}% vs baseline")' "$BASELINE_MEDIAN" "$median")"
    echo "   vs baseline: ${delta}"
    RESULT_LINES="${RESULT_LINES}| \`${value}\` | ${median} | ${backend} | ${delta} |"$'\n'
  fi
done

echo
echo "📊 Median wall-clock (${KNOB}, ${REPS} reps, --gpu ${GPU_MODE})"
echo
echo "| Value | Wall (s) | gpuBackend | Delta |"
echo "|---|---|---|---|"
printf '%s' "$RESULT_LINES"
