## Summary

Make the CI workflow graph explicit and the merge gate stable. `ci.yml` now
declares a purposeful `needs:` graph and a single fan-in aggregator job
(`CI Required Checks`) that branch protection can pin as the one required
check. Re-runs and partial failures are deterministic: the aggregator uses
`if: always()` and fails unless every upstream reports `success` or
`skipped`. Closes #23.

## Graph

```
validation ──┬── quality ─────────────┐
             │                         │
             └── security ─────────────┤
                                       ├──► ci-required  (aggregator)
shell-checks ──────────────────────────┤
spell-check ───────────────────────────┘
```

* `validation` is the foundational layout check — it runs first so a broken
  repo layout never spins up a Rust compile or a security audit.
* `quality` and `security` now `needs: [validation]`.
* `shell-checks` and `spell-check` stay independent and run in parallel.
* `ci-required` fans in all gating jobs, inspects `needs.<job>.result`
  explicitly, and is the single required check for branch protection.

## Evidence

Backend/CI-only change — no web UI to screenshot. Evidence is the bats
suite and the YAML validator:

* `scripts/check-ci-job-graph.sh` parses `ci.yml` and enforces the rules
  (expected jobs defined, `needs:` edges present, aggregator uses
  `if: always()` and checks `needs.*.result`). Wired into `quality.sh`.
* `./quality.sh` runs green locally (60/60 bats tests, 28 Rust unit tests,
  3 TDD tests, 4 smoke tests).
* `python3 -c "import yaml; ..."` confirms the `jobs` map and `needs`
  edges match the design exactly:

  ```
  quality ['validation']
  security ['validation']
  validation None
  shell-checks None
  spell-check None
  ci-required ['validation', 'quality', 'security', 'shell-checks', 'spell-check']
  ```

## Test Plan

* Added `tests/scripts/workflow_job_graph.bats` — 9 cases covering:
  * Happy path: a synthetic workflow with the expected graph passes.
  * Missing aggregator job fails with a clear error.
  * Missing `needs: [validation]` on `quality` fails.
  * Missing `if: always()` on the aggregator fails.
  * Aggregator without `needs.*.result` inspection fails.
  * Aggregator missing a gating job fails with the specific job name.
  * Missing workflow file and unknown flag yield descriptive errors.
  * The real repository `ci.yml` satisfies every rule.
* Existing 51 bats tests continue to pass unchanged.
* Post-merge verification: on the PR itself, confirm `CI Required Checks`
  appears as a status check; once green, update branch protection (human
  step) to require only that one check.
