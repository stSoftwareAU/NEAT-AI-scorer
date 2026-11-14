## Summary

Added an opt-in cached-halving experiment with four controls, frozen input
manifests, authoritative rejection audits and paired machine-readable results.
The ordinary scorer CLI and defaults remain unchanged. Representative-data
validation is outstanding. Part of #680.

- [x] Resume and verify the checkpoint; complete controls and benchmark command.
- [x] Pin the method, freeze the protocol and archive synthetic observations.
- [x] Run correctness, existing regression and full quality checks.
- [ ] Run representative-population/corpus comparisons.

## Spec

### Intent and Rationale

- Test whether reusing additive loss sums saves evaluations without losing
  full-data winners; a nested uncached control separates caching from sampling.
- This delivers experimental infrastructure and correctness evidence, not a
  deployed optimisation or a full reproduction of EvalAlloc.

### Essential Design Decisions

- Keep the ordinary positional CLI and candidate files unchanged. The separate
  worker reuses the directory loader, streaming decoder, `SampleSpec` and
  canonical cost accumulation; independent streaming full scoring is authoritative.
- The stride arm adds this experiment's halving schedule to existing sampling.
  `sampling.rs`, `multi_score.rs` and `racing_stdio.rs` provide no shipped
  successive-halving controller or externally defined promotion policy.
- Freeze rates, eta, tolerance, thresholds, hashes, seeds, cohorts, repetitions
  and limits before scoring. Algorithm constants come from the native
  `--describe-protocol`; the driver keeps no second rate/tolerance table.
- Use the predeclared `1e-6` relative tolerance for batch/worker regrouping,
  with denominator `max(abs(authoritative), 1e-12)`. Report promotion disagreement
  and every rejected candidate's authoritative error rather than hiding them.

### Undiscoverable Facts

- No representative candidates or corpus were supplied/configured for this
  run; production-fixture environment variables were unset.
- Measurements are single-host observations from `vibe-coder-7663` on
  Linux/aarch64. Precise CPU model information was unavailable. Shared-host
  timings are indicative and cannot establish production performance.

## Evidence

- #680: Experiment: Benchmark shared-core cached successive-halving evaluation.
- [Protocol and reproducible commands](../../experiments/halving.md), including
  the pinned [arXiv:2610.37276v1 method](https://arxiv.org/html/2610.37276v1).
- [Frozen manifest](../../evidence/issue-680-manifest.json) and
  [raw observations, gzip JSONL](../../evidence/issue-680-synthetic.jsonl.gz):
  96/96 successful synthetic trials, no integrity failures or omitted trials.
  All cached oracle winners were retained, with zero regret; maximum surviving
  full-result relative deviation across the arms was `2.01e-14`.

Per-trial evaluations on the 4,096-record synthetic validation cases:

| Population | Full | Stride | Nested uncached | Cached |
| --- | ---: | ---: | ---: | ---: |
| 12 | 49,152 | 12,230 | 12,251 | 9,914 |
| 24 | 98,304 | 20,364 | 20,406 | 16,347 |

Caching saved approximately 19–20% of evaluations against the nested control
on these fixtures. Total experimental accounting includes 8,603,760 arm
evaluations plus 5,308,416 independent full-audit evaluations. Cache statistics
occupied 192/384 bytes; process peak RSS was 19.92–21.07 MiB.

Indicative shared-host cached-minus-stride time differences were -0.0264 ms
[-0.0298, -0.0222] and -0.0527 ms [-0.0558, -0.0485], respectively (paired means,
seed-cluster bootstrap 95% intervals). Raw distributions and preparation,
compilation, scoring, memory and audit costs are in the observations. The
scientific verdict is `inconclusive-no-representative-validation`.

```mermaid
flowchart LR
    M[Frozen manifest] --> A[Four paired replay arms]
    A --> F[Independent streaming full scorer]
    F --> R[Reject audits and versioned JSONL]
```

This is backend/CLI work; no visual surface changed.

## Acceptance Criteria

- **missing** — Deliver a reproducible command/manifest and machine-readable
  results with paired distributions, uncertainty, failures and actual coverage.
  Fixtures alone do not demonstrate an optimisation benefit.
  — reason: those artefacts are delivered for synthetic correctness fixtures;
  representative validation inputs/results and a production benefit remain
  unavailable and unverified.

## Test Plan

- `cargo test -p rust_scorer --lib halving::tests < /dev/null` — passed, 24 tests.
  `rust_scorer/src/halving.rs::tests` covers disjoint increments, eight canonical
  costs, exact counts, RMSE finalisation, ties, empty early rungs, invalid
  shapes, non-finite corpus/reference values and nested cached/uncached parity.
  Empty-rung, non-finite and shape tests were observed failing on the resumed
  implementation before their fixes; this is checkpoint review, not a claim
  of reproducing a default-branch bug.
- `cargo test -p rust_scorer --test halving_bench_cli < /dev/null` — passed,
  five tests in `rust_scorer/tests/halving_bench_cli.rs`. They exercise the real
  native worker and Python driver, authoritative RMSE parity, a deliberately
  rejected full-data winner with 0.96 regret, limits, malformed data, invalid
  order/seed, split hash overlap, changed inputs/binary and native failures.
  `frozen_driver_runs_real_worker_and_rejects_changed_inputs` also demonstrated
  the changed-binary guard failing before its ordering fix and passing after it.
- `python3 -B -m unittest discover -s tests/experiments -v < /dev/null` — passed,
  four tests in `tests/experiments/test_halving_driver.py`: counterbalancing,
  seed-cluster uncertainty, content/inventory hashes and protocol validation.
- `cargo test -p rust_scorer --test sample_rate_tdd --test racing_stdio_cli
  < /dev/null` — passed, 14 existing tests in
  `rust_scorer/tests/sample_rate_tdd.rs` and `rust_scorer/tests/racing_stdio_cli.rs`.
- `timeout 1200 ./quality.sh < /dev/null` — passed: shell checks/validators,
  spelling, shell tests, cargo-deny, formatting, clippy, check, builds, the
  workspace tests, rustdoc and release build.
- No base-branch test assertions were removed. In the resumed checkpoint's
  `rust_scorer/src/halving.rs::tests::verify_full_match_fails_loudly`, the error
  text expectation `tolerance 0.000000001` became `tolerance 0.000001` for the
  predeclared numeric contract; its negative fixture remains outside tolerance
  and its exact-match positive assertion remains intact.
- Existing directory loader callers retain their behaviour: CPU scoring, GPU
  compatibility/probing and GPU scoring. Its visibility and fields became
  crate-visible for reuse; loading semantics did not change. All four replay
  arms use the shared input checks and canonical loss helper. The driver reads
  algorithm constants from the native worker rather than mirroring them.
- Read the README Binaries section and the experiment's method, protocol,
  accounting and results sections; updated README, CHANGELOG, CONTRIBUTING and
  `docs/experiments/halving.md`. No workflow file changed.

## Security Check

The driver checks the executable hash before querying its protocol, verifies
input/source/environment consistency before scoring, and verifies again after
the run. Subprocesses use argument arrays, writes refuse overwrites, and
the scorer reads candidates without modifying them. The committed observations
contain public synthetic inputs and an empty tuning environment. No new
third-party library was introduced by this implementation.

## Merge Verification

CI and external review have not been observed.

## Limitations

Representative validation, quiet-host/200 MB Criterion evidence and macOS
execution were not run. Platform resource-probe failures and live kernel
loss-overflow failures were not injected. Packed serial replay timings exclude
per-rung disk I/O and do not measure production directory throughput. Synthetic
success establishes neither real selection quality nor an optimisation benefit.
