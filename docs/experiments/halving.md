# Cached cumulative halving experiment — Issue #680

This opt-in, CPU-only experiment replays immutable, fixed populations. It does
not change `rust_scorer`, deploy a policy, generate evolutionary proposals or
replace full-data acceptance. Public synthetic fixtures establish correctness;
representative validation is still outstanding. Performance changes follow the
[Performance Task Workflow](../../CONTRIBUTING.md#performance-task-workflow).

## Pinned method and adaptation

The reference is [arXiv:2610.37276v1, *Where Should the Evaluation Budget Go in
LLM-Driven Agent Scaffold Evolution?*](https://arxiv.org/html/2610.37276v1),
specifically Sections 5.1–5.3 and 6.1. EvalAlloc calibrates on fully evaluated
warm-up candidates, selects informative shared items, models responses, and
reuses observations during final successive halving. Its later rounds apply a
cross-fitted regression adjustment; the final round uses observed full scores.
Its reported four prefixes are 30/60/120/300 items, with 30/15/8/1 survivors.

The NEAT-AI adaptation isolates shared deterministic records and cached
additive losses at the requested 2%/5%/15%/100% rates. It uses raw canonical
losses, fixed populations and eta 2. It omits informative-core calibration,
response modelling, adaptive acquisition, cross-fitting, search feedback and
pre-existing search observations. This is a cached-halving ablation, **not a
reproduction of EvalAlloc**. The calibration cohort here is a separate protocol
check, not a learned response model.

## Controls and correctness

Inspection of `sampling.rs`, `multi_score.rs` and `racing_stdio.rs` found a
deterministic stride sampler and caller-controlled per-chunk early exit. This
repository has no shipped successive-halving controller. The stride control
therefore applies this experiment's promotion schedule to the existing
`SampleSpec`; it does not reproduce an external consumer's screening policy.

| Arm / driver label | Record selection | Reuse |
| --- | --- | --- |
| Full / `full` | Every record, every candidate | None |
| Stride / `stride` | Existing `SampleSpec` at each cumulative rate | Re-score each rung |
| Nested uncached / `nested` | Shared deterministic nested sets | Re-score each cumulative set |
| Cached / `cached` | The same nested sets | Evaluate disjoint increments; add sums/counts |

`record_rank(i, seed)` is a base-2 radical inverse of the wrapping index plus
seed, not a random permutation or an informativeness estimate. Thresholding
this rank gives disjoint increments whose union is the corpus. Shared IDs are
global across the native loader's ordered files. Actual rung record counts are
reported: small corpora need not hit the requested percentages exactly. Empty
initial rungs retain everyone; later promotions retain `ceil(alive / 2)`.
Ties go to the lower sorted candidate index. Ranking uses mean loss and omits
the ordinary scorer's complexity penalty.

```mermaid
flowchart LR
    F[Freeze models, data, protocol and limits] --> C[Separate calibration replay]
    C --> V[Untouched validation replay]
    V --> A[Four counterbalanced arms]
    A --> S[Independent streaming full scoring]
    S --> R[Audit every reject and surviving full result]
    R --> J[Raw JSONL and paired seed-level uncertainty]
```

All arms call `cost::accumulate_cost_sum`. Its eight built-in losses are
additive over records; RMSE caches the **squared-error sum**, then calls
`CostKind::finalise_mean` once on the cumulative sum/count. The experiment
accepts homogeneous input/output widths and forward-only candidates. It
rejects non-finite corpus values and loss sums. Inputs remain read-only.

Every trial independently runs the ordinary streaming CPU directory scorer
after the four arms. Its mean errors remain authoritative. Each arm's surviving
full errors must satisfy
`abs(replay - authoritative) / max(abs(authoritative), 1e-12) <= 1e-6`.
This predeclared tolerance allows batch/worker accumulation differences; it
does not guarantee identical rankings of nearly tied candidates. The nested
control agreement metric exposes differing promotion decisions. Audit output
includes authoritative errors for the whole population, each rejected index
and rung, lost full-data winners, better rejected candidates and regret.

## Frozen protocol

`prepare` creates a new manifest before calibration or validation scoring.
It records candidate and corpus SHA-256 inventories, source fingerprint, git
tip, executable hash, scorer and neat-core versions, Rust compiler, hardware,
tuning environment, input preparation and binary build costs. The default path
builds the release worker with `cargo build --locked`. `--binary` is a
development shortcut: it records build time as unknown and does not establish
the binary's association with the recorded source.
The driver obtains rung rates, eta and numeric tolerance from the native
worker's `--describe-protocol`; it does not maintain a second algorithm table.

The manifest contains cohort provenance, seeds, repetitions, population sizes
(the candidate inventory lengths), canonical cost and compute limits. It
rejects candidate or whole-file corpus hash overlap between calibration and
validation. This cannot detect duplicate records inside differently packaged
files: `splitJustification` must describe the underlying separation. No
parameters are fitted or thresholds adjusted after calibration. Real cohorts
must be frozen before inspecting validation losses.

Protocol revision `neat-halving-protocol/1` fixes these descriptive decision
thresholds before validation:

- Three or more seeds and at least two representative validation population
  sizes; the generated correctness fixture uses seeds 0, 17, 101, populations
  12 and 24, and eight repetitions.
- At least 95% retention of the deterministic full-data winner, maximum
  relative regret 0.001 using `max(abs(best_error), 1e-12)`, at least 50% fewer
  record evaluations than full, strictly fewer than nested uncached, and
  identical nested-control promotion decisions for the evaluated trials.
- Indicative timing evidence additionally requires the upper 95% bootstrap
  bound of cached-minus-stride and cached-minus-nested wall time to be below
  zero. This is an experimental classification, never a CI/build gate.

Threshold checks apply separately to each representative validation case;
retention is averaged over seeds/repetitions and the other quality/saving
checks use the worst observed trial. A failure, integrity mismatch or missing
trial prevents a complete result. A synthetic-only run is explicitly
inconclusive regardless of its counts and timings. The `kind` label is supplied
by the manifest author; the runner cannot certify representativeness.

Repetitions must be a positive multiple of four. The Williams order generated
from `[full, stride, cached, nested]`, with cyclic label shifts, puts each arm
in each position and balances directed adjacent pairs in every four trials.
No warm-up is discarded. Each seed/repetition uses the same candidate/data
hashes for all four arms, with fresh network clones per arm. Calibration cases
run before validation cases. Neither arm order nor thresholds depend on losses.

Raw observations preserve within-trial pairs. The summary reports paired time
differences, evaluation savings, regret, retention and control agreement, with
min/median/max and means. It averages repetitions within each seed before a
2,000-draw seed-cluster bootstrap (RNG seed 680) and reports percentile 95%
intervals. Three seeds offer limited uncertainty resolution; repeated timings
are not independent candidate populations.

## Reproduce the public correctness run

Requires Rust (the pinned toolchain), Cargo and Python 3.9 or later. Research
runs are opt-in and are not invoked by routine CI.

```bash
experiment_dir="$(mktemp -d)"
python3 -B scripts/bench-halving.py prepare \
  --synthetic-root "$experiment_dir/inputs" \
  --manifest "$experiment_dir/manifest.json"
python3 -B scripts/bench-halving.py run \
  --manifest "$experiment_dir/manifest.json" \
  --results "$experiment_dir/results.jsonl"
```

Generation refuses an existing fixture root; manifests/results use exclusive
creation and refuse overwrites. Regenerating from the same generator produces
the same candidate/data hashes; timestamps, executable hashes and timings may
differ. The synthetic cohorts use separate candidate seeds and input-index
ranges, with a known IF-tree oracle. They are deliberately favourable
correctness fixtures, not evidence about real selection quality.

For local representative inputs, supply a JSON configuration instead:

```json
{
  "schema": "neat-halving-config/1",
  "cost": "MSE",
  "seeds": [0, 17, 101],
  "repetitions": 8,
  "splitJustification": "Describe independently reserved populations and records here.",
  "cases": [
    {"id": "calibration-24", "split": "calibration", "kind": "representative", "creaturesDir": "calibration/models", "dataDir": "calibration/data"},
    {"id": "validation-24", "split": "validation", "kind": "representative", "creaturesDir": "validation-24/models", "dataDir": "validation-24/data"},
    {"id": "validation-48", "split": "validation", "kind": "representative", "creaturesDir": "validation-48/models", "dataDir": "validation-48/data"}
  ],
  "limits": {"maxCorpusBytes": 268435456, "maxRecordEvaluations": 1000000000, "trialSeconds": 120, "totalSeconds": 1800}
}
```

Paths are relative to the config file. Run `prepare --config config.json
--manifest manifest.json`, then `run` as above. Keep private configs,
manifests, model/corpus files and observations outside this public repository;
they contain local paths, hashes, model identifiers and loss information.
The runner fetches no private inputs and publishes nothing.

## Accounting, limits and failure reporting

The native worker reuses the existing directory loader, `find_bin_files`,
alignment guard, `for_each_read_chunk`, decoder and canonical losses. It
materialises the complete corpus for replay and refuses oversized inputs;
there is no silent truncation. Defaults are 256 MiB of corpus bytes, a
conservative six-times-full evaluation upper bound of one billion, 120 seconds
per native trial and 1,800 seconds across driver execution. Binary compilation
has its own 900-second bound. These are operation/time limits, not a hard RSS
cap. Larger networks can consume substantial memory even with a small corpus.

`neat-halving-observation/1` records loading, network compilation, network
cloning, sample construction, kernel scoring, cache updates and total arm
times, plus independent full-scoring and audit-comparison overhead. Its record
evaluation total **includes** the extra full-data audit. Cache bytes count
sum/count entries; packed-record and nested-index allocation capacities are
reported separately. Linux/macOS peak process RSS includes networks, corpus,
I/O buffers and temporary allocations; other platforms report it as unknown.
Peak RSS is process-wide, not attributable to individual arms. Capacity
metrics alone are not a complete allocator/peak-memory measurement.

`neat-halving-results/1` is the final record in a JSONL stream: the first line
embeds the frozen manifest, each subsequent trial includes native output or a
failure, and the last line summarises actual coverage and paired comparisons.
Driver wall time includes startup, verification, process I/O and aggregation
up to summary construction. `prepareSecs` includes building, hashing and, for
`--synthetic-root`, fixture generation. Component build/generation times must
not be added to that total again. Externally supplied input preparation is
unknown unless declared in `inputPreparationSecs` in the config. There is no
search/model-fitting cost in this fixed-population experiment.

Inputs, executable, source and tuning environment are verified before and
after the driver run; files must remain static during it. This is hash-based
verification, not an atomic filesystem snapshot. A preflight refusal exits
non-zero with a diagnostic before creating results. Executed trial errors and
timeouts are recorded as failures, never substituted with successful data;
partial evaluation counts are explicitly unknown. Incomplete runs exit
non-zero. A complete but negative/inconclusive research result exits zero,
with its scientific verdict distinct from execution success.

Packed replay excludes per-rung disk I/O and uses serial candidate scoring;
the independent authority uses ordinary streaming/worker behaviour. These
timings cannot establish an end-to-end speed-up of production directory
scoring. OS caches, shared-host load, small synthetic networks and few seeds
further limit interpretation. No optimisation benefit is established here.

## Correctness checks

```bash
cargo test -p rust_scorer --lib halving::tests
cargo test -p rust_scorer --test halving_bench_cli
python3 -B -m unittest discover -s tests/experiments -v
```

These checks cover disjoint increments, canonical costs including RMSE,
counts, nested-control parity, ties, empty initial rungs, malformed/non-finite
inputs, authoritative full-score parity, a deliberately lost winner, frozen
input changes, split overlap, native failures and counterbalancing/statistics.
The ordinary sampling/racing regression suites remain the safety net for
existing interfaces. See the committed issue summary for observed commands and
the public evidence record for the measured scope.
