#!/usr/bin/env bats
# Tests for scripts/bench-knob-sweep.sh — Issue #545.
#
# The real harness scores a production corpus, so the scorer binary is shimmed
# on disk and the inputs kept tiny. We assert the observable contract:
#   * missing corpus/creature inputs skip cleanly (exit 0) — the Issue #448
#     convention shared with scripts/bench-shallow-gpu.sh,
#   * supplied-but-unreadable inputs fail loud (non-zero) — never a silent pass,
#   * the swept knob reaches the scorer's environment at each value, and is
#     unset for the literal `default`,
#   * every value runs the requested number of repetitions,
#   * a non-zero scorer exit propagates,
#   * the host report and the median/delta table are emitted.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/bench-knob-sweep.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP="$(mktemp -d)"
  export TMP
  SCORER_LOG="${TMP}/scorer-log"
  export SCORER_LOG

  # Shim scorer: answers --host-report, otherwise logs the swept knob value and
  # emits a plausible directory-mode JSON body.
  cat > "${TMP}/scorer" << EOF
#!/bin/bash
if [ "\$1" = "--host-report" ]; then
  echo '{"schema":"neat-scorer-host-report/1","logical_cpus":4,"knobs":{}}'
  exit "\${SHIM_REPORT_EXIT:-0}"
fi
echo "ARGS: \$* KNOB=\${NEAT_SCORER_READ_BYTES-unset} THREADS=\${NEAT_SCORER_ACTIVATION_THREADS-unset}" >> "${SCORER_LOG}"
echo '{"c-000":{"score":-0.1,"error":1.0,"recordCount":8,"gpuBackend":"cpu-fallback"}}'
exit "\${SHIM_SCORER_EXIT:-0}"
EOF
  chmod +x "${TMP}/scorer"

  mkdir -p "${TMP}/creatures" "${TMP}/data"
  printf 'x' > "${TMP}/data/0.bin"
  printf '{}' > "${TMP}/creatures/c-000.json"

  export BENCH_SWEEP_SCORER="${TMP}/scorer"
  export BENCH_SWEEP_REPS=2
}

teardown() {
  rm -rf "$TMP"
}

@test "skips cleanly when the creature input is unset" {
  run env -u BENCH_SWEEP_CREATURE BENCH_SWEEP_DATA="${TMP}/data" "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" == *"BENCH_SWEEP_CREATURE"* ]]
  [[ "$output" == *"skip"* ]]
  [ ! -f "$SCORER_LOG" ]
}

@test "skips cleanly when the corpus input is unset" {
  run env -u BENCH_SWEEP_DATA BENCH_SWEEP_CREATURE="${TMP}/creatures" "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" == *"skip"* ]]
  [ ! -f "$SCORER_LOG" ]
}

@test "fails loud when the creature input does not exist" {
  BENCH_SWEEP_CREATURE="${TMP}/missing" BENCH_SWEEP_DATA="${TMP}/data" \
    run "$SCRIPT_UNDER_TEST"
  [ "$status" -ne 0 ]
  [[ "$output" == *"missing"* ]]
}

@test "fails loud when the corpus directory does not exist" {
  BENCH_SWEEP_CREATURE="${TMP}/creatures" BENCH_SWEEP_DATA="${TMP}/no-such-corpus" \
    run "$SCRIPT_UNDER_TEST"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no-such-corpus"* ]]
}

@test "prints the host report before sweeping" {
  BENCH_SWEEP_CREATURE="${TMP}/creatures" BENCH_SWEEP_DATA="${TMP}/data" \
    run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" == *"neat-scorer-host-report/1"* ]]
}

@test "fails loud when the scorer cannot produce a host report" {
  SHIM_REPORT_EXIT=4 BENCH_SWEEP_CREATURE="${TMP}/creatures" \
    BENCH_SWEEP_DATA="${TMP}/data" run "$SCRIPT_UNDER_TEST"
  [ "$status" -ne 0 ]
  [[ "$output" == *"host-report"* ]]
}

@test "runs every value for the requested repetitions" {
  BENCH_SWEEP_CREATURE="${TMP}/creatures" BENCH_SWEEP_DATA="${TMP}/data" \
    BENCH_SWEEP_VALUES="2097152,8388608" run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [ "$(grep -c 'KNOB=2097152' "$SCORER_LOG")" -eq 2 ]
  [ "$(grep -c 'KNOB=8388608' "$SCORER_LOG")" -eq 2 ]
}

@test "the literal default value runs with the knob unset" {
  BENCH_SWEEP_CREATURE="${TMP}/creatures" BENCH_SWEEP_DATA="${TMP}/data" \
    BENCH_SWEEP_VALUES="default" NEAT_SCORER_READ_BYTES=999 run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [ "$(grep -c 'KNOB=unset' "$SCORER_LOG")" -eq 2 ]
}

@test "sweeps a knob other than the read-bytes default" {
  BENCH_SWEEP_CREATURE="${TMP}/creatures" BENCH_SWEEP_DATA="${TMP}/data" \
    BENCH_SWEEP_KNOB=NEAT_SCORER_ACTIVATION_THREADS BENCH_SWEEP_VALUES="4" \
    run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [ "$(grep -c 'THREADS=4' "$SCORER_LOG")" -eq 2 ]
}

@test "invokes the scorer with the creature and data paths and the gpu mode" {
  BENCH_SWEEP_CREATURE="${TMP}/creatures" BENCH_SWEEP_DATA="${TMP}/data" \
    BENCH_SWEEP_GPU=off BENCH_SWEEP_VALUES="default" run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  grep -q -- "--gpu off ${TMP}/creatures ${TMP}/data" "$SCORER_LOG"
}

@test "rejects a knob outside the NEAT_SCORER_ namespace" {
  BENCH_SWEEP_CREATURE="${TMP}/creatures" BENCH_SWEEP_DATA="${TMP}/data" \
    BENCH_SWEEP_KNOB="PATH" run "$SCRIPT_UNDER_TEST"
  [ "$status" -ne 0 ]
  [[ "$output" == *"NEAT_SCORER"* ]]
  [ ! -f "$SCORER_LOG" ]
}

@test "rejects a non-numeric sweep value" {
  BENCH_SWEEP_CREATURE="${TMP}/creatures" BENCH_SWEEP_DATA="${TMP}/data" \
    BENCH_SWEEP_VALUES="2097152,lots" run "$SCRIPT_UNDER_TEST"
  [ "$status" -ne 0 ]
  [[ "$output" == *"lots"* ]]
  [ ! -f "$SCORER_LOG" ]
}

@test "rejects an invalid gpu mode" {
  BENCH_SWEEP_CREATURE="${TMP}/creatures" BENCH_SWEEP_DATA="${TMP}/data" \
    BENCH_SWEEP_GPU="yolo" run "$SCRIPT_UNDER_TEST"
  [ "$status" -ne 0 ]
  [[ "$output" == *"yolo"* ]]
}

@test "propagates a non-zero scorer exit instead of reporting success" {
  SHIM_SCORER_EXIT=3 BENCH_SWEEP_CREATURE="${TMP}/creatures" \
    BENCH_SWEEP_DATA="${TMP}/data" run "$SCRIPT_UNDER_TEST"
  [ "$status" -ne 0 ]
  [[ "$output" == *"exit 3"* ]]
}

@test "prints a median per value and the delta against the first value" {
  BENCH_SWEEP_CREATURE="${TMP}/creatures" BENCH_SWEEP_DATA="${TMP}/data" \
    BENCH_SWEEP_VALUES="default,2097152" run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" == *"median"* ]]
  [[ "$output" == *"vs baseline"* ]]
  [[ "$output" == *"| Value | Wall (s) | gpuBackend | Delta |"* ]]
}
