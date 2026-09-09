# Check the Semgrep container pin for bump-ability (Issue #602)

## Summary

`.github/workflows/semgrep.yml` pins the scanner container to a bare
`semgrep/semgrep@sha256:…` digest with no tag beside it. The digest is
immutable, but Renovate's `docker` manager and Dependabot both resolve a
container bump from the **tag** component — with no tag there is nothing to
resolve, so the pin silently freezes at whatever the workflow comment records
(Semgrep v1.86.0, frozen 2026-05-18) and no later scan flags it as behind.

The one-line YAML edit that fixes it **cannot be made by this worker**: the
automation credentials carry no `workflow` OAuth scope, so both `git push` and
the Contents API refuse any change under `.github/workflows/` (evidence below).
Per [Human escalation](../../../CONTRIBUTING.md#human-escalation) this PR lands
everything that does *not* need the workflow change, and the maintainer edit is
spelled out below and on the issue.

What landed:

- **`scripts/check-semgrep-workflow.sh` now accepts the canonical
  `semgrep/semgrep:<version>@sha256:<digest>` pin.** It previously **rejected**
  that exact form as "not pinned by digest" (the tag branch matched first), so
  the repository's own gate blocked the fix the issue asks for. The guard now
  names the tag it found, rejects `:latest@sha256:…` (a floating tag resolves to
  nothing an updater can compare), and reports a tagless digest as a `WARN` line
  on every gate run — visible, never folded into a silent pass.
- **New shared `warn` primitive in `scripts/lib/check-harness.sh`** — stderr,
  `EXIT_CODE` untouched — for a deficiency whose fix is blocked outside the
  repository. The `ok`/`fail` protocol is owned by the harness, so the third
  reporting level belongs there rather than being open-coded in one validator.
- README and CHANGELOG record the canonical pin shape and why the tag matters.

**Maintainer action required** — one line in `.github/workflows/semgrep.yml:53`:

```yaml
      image: semgrep/semgrep:1.86.0@sha256:a9ea2d5621c29d815d90c2a3b2f9571da8972ef4ff855c9e4902681730240e35
```

The digest is unchanged, so the image stays byte-for-byte identical; only the
resolvable version tag is added. (Bumping to a newer Semgrep release at the same
time is fine — update the tag, the digest and the version-label comment in
lockstep, per the workflow's own bump protocol.) The `WARN` line clears and the
guard reports `… carries the bump-able version tag ':1.86.0'` the moment that
edit lands.

Closes #602.

## Evidence

Backend/CI-only change — no web interface to screenshot. Verified by the BATS
suites and by running the validator against the shipped workflow.

Push refusal, reproduced twice on this run (this is why the YAML edit is not in
the diff):

```text
! [remote rejected] … (refusing to allow an OAuth App to create or update
  workflow `.github/workflows/semgrep.yml` without `workflow` scope)
```

The Contents API refuses the same write (HTTP 404 on
`PUT …/contents/.github/workflows/semgrep.yml`) while an identical `PUT` to a
non-workflow path on the same probe branch succeeds — so the refusal is scope-
specific, not a missing file or a bad request. The probe branch was deleted.

Validator against the shipped workflow today:

```text
OK   …/semgrep.yml: Semgrep container image is pinned by digest (semgrep/semgrep@sha256:<digest>)
WARN …/semgrep.yml: Semgrep container image digest carries no version tag — Renovate's
     docker manager and Dependabot resolve bumps from the tag component, so this pin can
     never be bumped automatically; use semgrep/semgrep:<version>@sha256:<digest> (Issue #602)
```

```mermaid
flowchart LR
    A["semgrep/semgrep<br/>(bare)"] --> X[FAIL: not pinned]
    B["semgrep/semgrep:latest"] --> X
    C["semgrep/semgrep:1.86.0<br/>(tag only, mutable)"] --> X
    D["semgrep/semgrep:latest@sha256:…"] --> X2["FAIL: floating tag,<br/>nothing to resolve"]
    E["semgrep/semgrep@sha256:…<br/>(tagless digest — today)"] --> W["OK + WARN:<br/>immutable but un-bumpable"]
    F["semgrep/semgrep:1.86.0@sha256:…<br/>(canonical)"] --> Y["OK: immutable and<br/>bump-able"]
```

## Test Plan

- `tests/scripts/semgrep_workflow.bats` — the container fixture now uses the
  canonical `:<tag>@sha256:<digest>` pin (the old fixture, and every mutation
  test derived from it, would have failed against the new rule), plus three new
  cases:
  - `names the bump-able version tag beside the digest (issue #602)` — passes and
    reports the tag, with no `WARN`.
  - `warns but still passes when the digest pin carries no version tag (issue #602)`
    — exit 0, `WARN`, no `FAIL`.
  - `fails when the tag beside the digest is :latest (issue #602)` — non-zero.
  - Existing cases (bare name, `:latest`, tag-only, malformed digest, missing
    entry point, `--config`, token wiring, rationale comment) all still fail as
    before; `real repository semgrep workflow satisfies every rule` still passes.
- `tests/scripts/check_harness.bats` —
  `warn reports on stderr without flipping the exit code` drives the new
  primitive through a throwaway validator and asserts the stream and exit code.
- Full local suite: `bats tests/scripts` — 629 pass. The one failure,
  `living docs contain no box-drawing ASCII diagrams`, is **pre-existing** and
  environmental: it fails identically on the unmodified base commit because this
  container has no `en_US.UTF-8` locale (the test warns
  `setlocale: LC_ALL: cannot change locale`). CI provides the locale.
- `shellcheck -x -s bash` clean on both changed scripts; `markdownlint-cli2`
  clean on the changed Markdown.
