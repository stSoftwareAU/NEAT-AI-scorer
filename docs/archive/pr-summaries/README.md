# PR summary archive

**PR summaries live here — `docs/archive/pr-summaries/`, one file per PR,
named `pr-summary-<issue>.md`.** This directory is the single home for the
archive; nothing goes in the `docs/` root (Issue #508).

The summaries are a **frozen historical record** of merged PRs and the
project's durable cross-machine memory: they capture what was tried, what
shipped, and — just as importantly — what did **not** work (for example the
single-creature GPU negative result). Read them before re-attempting an
approach; do not rewrite them, because that falsifies the record.

Durable learnings that still apply to the current code belong in the living
docs — [`README.md`](../../../README.md),
[`CONTRIBUTING.md`](../../../CONTRIBUTING.md) and
[`docs/performance-baseline.md`](../../performance-baseline.md) — not only in a
summary. A summary explains one PR; the living docs explain the project.

Two gates keep the convention honest:

- `scripts/check-pr-summary-archive.sh` (in `./quality.sh`) fails when a
  summary lands outside this directory, when the convention doc is missing, or
  when the `.codespellrc` skip list stops covering the archive.
- The `.codespellrc` `skip` entry exempts this directory from codespell, as the
  summaries quote typo fixtures from the PRs they describe (Issue #21).
