#!/usr/bin/env bats
# Tests for scripts/check-rmse-docs.sh — Issue #556.
#
# The README once described `RMSE` as ranking "identically to MSE", which reads
# as "RMSE is redundant". The truth is narrower: `sqrt` is monotonic, so the
# creature *ordering* matches MSE while the *reported score* genuinely differs
# (it is in the target's own units). This gate keeps that distinction in both
# the README cost-selector section and the `CostKind::Rmse` rustdoc.
#
# Synthetic fixtures in a temp directory exercise the pass/fail behaviour, so
# the real documents are never mutated by the tests.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-rmse-docs.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_DIR="$(mktemp -d)"
  export TMP_DIR

  write_readme "$TMP_DIR/README.md"
  write_source "$TMP_DIR/cost.rs"
}

teardown() {
  rm -rf "$TMP_DIR"
}

run_check() {
  run "$SCRIPT_UNDER_TEST" \
    --readme "$TMP_DIR/README.md" \
    --source "$TMP_DIR/cost.rs"
}

# README fixture carrying the clarified wording the gate demands.
write_readme() {
  cat >"$1" <<'EOF'
# Title

### Cost function selector (Issues #120, #121)

| Value  | Meaning                                                          |
|--------|------------------------------------------------------------------|
| `MSE`  | Mean Squared Error                                               |
| `RMSE` | Root Mean Squared Error — same creature ordering as MSE, but a different reported score, in the target's own units |

`RMSE` reuses the MSE squared-error accumulation and differs only by a
host-side `sqrt` at finalisation. Because `sqrt` is monotonic it preserves the
creature ordering `MSE` produces; the reported score differs, being expressed
in the target's own units.

## Next section

Unrelated prose.
EOF
}

# Minimal stand-in for rust_scorer/src/cost.rs.
write_source() {
  cat >"$1" <<'EOF'
pub enum CostKind {
    /// Mean Squared Error.
    #[value(name = "MSE")]
    Mse,
    /// Root Mean Squared Error. Monotonic `sqrt` of the MSE mean, so the
    /// creature ordering matches MSE while the reported score differs — it is
    /// in the target's own units.
    #[value(name = "RMSE")]
    Rmse,
    /// Mean Absolute Error.
    #[value(name = "MAE")]
    Mae,
}
EOF
}

@test "passes on documents that separate ordering from reported magnitude" {
  run_check
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}

@test "fails when the README table row revives 'ranks identically to MSE'" {
  sed -i.bak 's/same creature ordering as MSE, but a different reported score, in the target'"'"'s own units/ranks identically to MSE, reports same-unit magnitudes/' \
    "$TMP_DIR/README.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"identical"* ]]
}

@test "fails when the README prose claims it ranks creatures identically" {
  sed -i.bak 's/it preserves the/it ranks creatures identically to `MSE`, preserving the/' \
    "$TMP_DIR/README.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"identical"* ]]
}

@test "fails when the RMSE table row omits that the reported score differs" {
  sed -i.bak 's/, but a different reported score, in the target'"'"'s own units//' \
    "$TMP_DIR/README.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"reported score"* ]]
}

@test "fails when the RMSE table row omits the ordering fact" {
  sed -i.bak 's/same creature ordering as MSE, but a different/a different/' \
    "$TMP_DIR/README.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"ordering"* ]]
}

@test "fails when the README prose never explains why the ordering matches" {
  sed -i.bak 's/Because `sqrt` is monotonic it/It/' "$TMP_DIR/README.md"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"monotonic"* ]]
}

@test "fails when the cost-selector section is absent entirely" {
  cat >"$TMP_DIR/README.md" <<'EOF'
# Title

No cost selector documented here.
EOF
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"could not find"* ]]
}

@test "fails when the RMSE rustdoc claims identical ranking" {
  sed -i.bak 's|/// creature ordering matches MSE while the reported score differs — it is|/// variant ranks identically to MSE, so the score is|' \
    "$TMP_DIR/cost.rs"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"identical"* ]]
}

@test "fails when the RMSE rustdoc drops the differing-magnitude fact" {
  sed -i.bak 's|/// in the target'"'"'s own units\.|///|' "$TMP_DIR/cost.rs"
  run_check
  [ "$status" -ne 0 ]
  [[ "$output" == *"units"* ]]
}

@test "reports an error when a document is missing" {
  rm -f "$TMP_DIR/README.md"
  run_check
  [ "$status" -eq 2 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits 2" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -eq 2 ]
  [[ "$output" == *"Usage"* ]]
}

@test "a flag without a value exits 2" {
  run "$SCRIPT_UNDER_TEST" --readme
  [ "$status" -eq 2 ]
}

@test "the real repository documents satisfy the check" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
