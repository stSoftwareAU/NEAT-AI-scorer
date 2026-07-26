#!/usr/bin/env bats
# Tests for scripts/bench-shallow-gpu.sh — Issue #467.
#
# The real harness scores a multi-hundred-MB synthetic corpus, so the scorer
# binary is shimmed on disk and the fixture kept tiny. We assert the observable
# contract:
#   * a missing BENCH_SHALLOW_CREATURE skips cleanly (exit 0) with guidance,
#   * a creature path that does not exist fails loud (non-zero),
#   * every requested mode runs the requested number of repetitions,
#   * the scorer is invoked with `--gpu <mode> <creatures_dir> <data_dir>`,
#   * a non-zero scorer exit propagates (never reconciled as success),
#   * the summary table reports a median per mode and the GPU-vs-CPU delta.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/bench-shallow-gpu.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP="$(mktemp -d)"
  export TMP
  SCORER_LOG="${TMP}/scorer-log"
  export SCORER_LOG

  # Shim scorer: logs argv, emits a plausible JSON body on stdout.
  cat > "${TMP}/scorer" << EOF
#!/bin/bash
echo "ARGS: \$*" >> "${SCORER_LOG}"
echo '{"c-000":{"score":-0.1,"error":1.0,"recordCount":8,"gpuBackend":"metal"}}'
exit "\${SHIM_SCORER_EXIT:-0}"
EOF
  chmod +x "${TMP}/scorer"

  # Minimal shallow creature: 4 inputs, 2 hidden, 1 output.
  cat > "${TMP}/creature.json" << 'EOF'
{"input":4,"output":1,"forwardOnly":true,"semanticVersion":"4.0.0",
 "neurons":[{"type":"hidden","uuid":"h0","bias":0.0,"squash":"TANH"},
            {"type":"hidden","uuid":"h1","bias":0.0,"squash":"TANH"},
            {"type":"output","uuid":"o0","bias":0.0,"squash":"IDENTITY"}],
 "synapses":[{"fromUUID":"input-0","toUUID":"h0","weight":0.5},
             {"fromUUID":"input-1","toUUID":"h1","weight":0.5},
             {"fromUUID":"h0","toUUID":"o0","weight":0.5},
             {"fromUUID":"h1","toUUID":"o0","weight":0.5}]}
EOF

  export BENCH_SHALLOW_SCORER="${TMP}/scorer"
  export BENCH_SHALLOW_FIXTURE_DIR="${TMP}/fixture"
  export BENCH_SHALLOW_N=2
  export BENCH_SHALLOW_BINS=2
  export BENCH_SHALLOW_RECORDS=8
  export BENCH_SHALLOW_REPS=2
}

teardown() {
  rm -rf "$TMP"
}

@test "skips cleanly when BENCH_SHALLOW_CREATURE is unset" {
  run env -u BENCH_SHALLOW_CREATURE "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" == *"BENCH_SHALLOW_CREATURE"* ]]
  [[ "$output" == *"skip"* ]]
  [ ! -f "$SCORER_LOG" ]
}

@test "fails loud when the creature path does not exist" {
  BENCH_SHALLOW_CREATURE="${TMP}/missing.json" run "$SCRIPT_UNDER_TEST"
  [ "$status" -ne 0 ]
  [[ "$output" == *"missing.json"* ]]
}

@test "runs every mode for the requested repetitions" {
  BENCH_SHALLOW_CREATURE="${TMP}/creature.json" \
    BENCH_SHALLOW_MODES="off on" run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [ "$(grep -c -- '--gpu off' "$SCORER_LOG")" -eq 2 ]
  [ "$(grep -c -- '--gpu on' "$SCORER_LOG")" -eq 2 ]
}

@test "invokes the scorer with the staged creature and data directories" {
  BENCH_SHALLOW_CREATURE="${TMP}/creature.json" \
    BENCH_SHALLOW_MODES="off" run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  grep -q -- "--gpu off ${BENCH_SHALLOW_FIXTURE_DIR}/creatures ${BENCH_SHALLOW_FIXTURE_DIR}/data" \
    "$SCORER_LOG"
}

@test "stages BENCH_SHALLOW_N creature copies and BENCH_SHALLOW_BINS bin files" {
  BENCH_SHALLOW_CREATURE="${TMP}/creature.json" BENCH_SHALLOW_KEEP=1 \
    BENCH_SHALLOW_MODES="off" run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [ "$(find "${BENCH_SHALLOW_FIXTURE_DIR}/creatures" -name '*.json' | wc -l | tr -d ' ')" -eq 2 ]
  [ "$(find "${BENCH_SHALLOW_FIXTURE_DIR}/data" -name '*.bin' | wc -l | tr -d ' ')" -eq 2 ]
}

@test "generated records match the creature input+output width" {
  BENCH_SHALLOW_CREATURE="${TMP}/creature.json" BENCH_SHALLOW_KEEP=1 \
    BENCH_SHALLOW_MODES="off" run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  # 8 records total over 2 bins => 4 records per bin, 5 floats each => 80 bytes.
  size="$(wc -c < "${BENCH_SHALLOW_FIXTURE_DIR}/data/0.bin" | tr -d ' ')"
  [ "$size" -eq 80 ]
}

@test "propagates a non-zero scorer exit instead of reporting success" {
  SHIM_SCORER_EXIT=3 BENCH_SHALLOW_CREATURE="${TMP}/creature.json" \
    BENCH_SHALLOW_MODES="off" run "$SCRIPT_UNDER_TEST"
  [ "$status" -ne 0 ]
  [[ "$output" == *"exit 3"* ]]
}

@test "prints a median per mode and the GPU-vs-CPU delta" {
  BENCH_SHALLOW_CREATURE="${TMP}/creature.json" \
    BENCH_SHALLOW_MODES="off on" run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" == *"median"* ]]
  [[ "$output" == *"gpu on vs gpu off"* ]]
}

@test "stages every creature listed in a comma-separated BENCH_SHALLOW_CREATURE" {
  cp "${TMP}/creature.json" "${TMP}/twin.json"
  BENCH_SHALLOW_CREATURE="${TMP}/creature.json,${TMP}/twin.json" BENCH_SHALLOW_KEEP=1 \
    BENCH_SHALLOW_MODES="off" run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [ "$(find "${BENCH_SHALLOW_FIXTURE_DIR}/creatures" -name 'creature-*.json' | wc -l | tr -d ' ')" -eq 1 ]
  [ "$(find "${BENCH_SHALLOW_FIXTURE_DIR}/creatures" -name 'twin-*.json' | wc -l | tr -d ' ')" -eq 1 ]
}

@test "rejects creatures whose input/output shape disagrees" {
  sed 's/"input":4/"input":5/' "${TMP}/creature.json" > "${TMP}/other.json"
  BENCH_SHALLOW_CREATURE="${TMP}/creature.json,${TMP}/other.json" \
    BENCH_SHALLOW_MODES="off" run "$SCRIPT_UNDER_TEST"
  [ "$status" -ne 0 ]
  [[ "$output" == *"shape"* ]]
}
