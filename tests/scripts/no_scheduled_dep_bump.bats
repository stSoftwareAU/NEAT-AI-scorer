#!/usr/bin/env bats
# Regression for Issue #105 — "I don't want a weekly dependency updates".
#
# The repository used to ship a weekly `upgrade-dependencies.yml` workflow
# (cron: "0 6 * * 1") that ran `cargo upgrade` and opened a PR every
# Monday. The owner now wants dependency bumps to happen only when a PR
# is being raised — i.e. via `bump-deps.sh` invoked from `quality.sh`
# during normal PR preparation — so the scheduled workflow and its two
# supporting scripts/tests have been removed.
#
# These tests fail if anything reintroduces the scheduled bump.

setup() {
  REPO_ROOT="$(cd "${BATS_TEST_DIRNAME}/../.." && pwd)"
  export REPO_ROOT
}

@test "the weekly upgrade-dependencies workflow does not exist" {
  [ ! -e "$REPO_ROOT/.github/workflows/upgrade-dependencies.yml" ]
}

@test "the workflow validator helper for the removed schedule is gone" {
  [ ! -e "$REPO_ROOT/scripts/check-upgrade-deps-workflow.sh" ]
}

@test "the manifest-change helper for the removed schedule is gone" {
  [ ! -e "$REPO_ROOT/scripts/check-upgrade-has-changes.sh" ]
}

@test "no workflow under .github/workflows schedules a cargo dependency bump" {
  # Walk every workflow and reject any cron-triggered job that names
  # `cargo upgrade` or `bump-deps.sh`. A `cargo audit` cron is fine —
  # that is the standalone security workflow.
  local found_offender=0
  while IFS= read -r wf; do
    [ -f "$wf" ] || continue
    # Skip files that have no `schedule:` trigger at all.
    if ! grep -qE '^[[:space:]]*schedule:' "$wf"; then
      continue
    fi
    if grep -qE '(cargo[[:space:]]+upgrade|bump-deps\.sh)' "$wf"; then
      echo "Offending scheduled workflow: $wf" >&2
      found_offender=1
    fi
  done < <(find "$REPO_ROOT/.github/workflows" -maxdepth 1 -name '*.yml' -type f)
  [ "$found_offender" -eq 0 ]
}

@test "quality.sh no longer invokes the removed upgrade-deps validator" {
  run grep -F "check-upgrade-deps-workflow.sh" "$REPO_ROOT/quality.sh"
  [ "$status" -ne 0 ]
}
