# Re-pin the Semgrep container by release tag + digest (Issue #640)

## Summary

`.github/workflows/semgrep.yml` pinned the scanner container as a bare
`semgrep/semgrep@sha256:a9ea2d56…`. The digest is immutable, so the image could
never be silently swapped — but it was also untrackable: Renovate's
`github-actions` manager and Dependabot's `docker` ecosystem resolve a version
bump from the **tag** and then rewrite the digest beside it, so a tagless pin
has nothing to resolve and can never be flagged as behind.

The pin is now `semgrep/semgrep:1.86.0@sha256:a9ea2d56…` — byte-for-byte the
same image, now bumpable. The tag was resolved against the upstream registry in
this run (`registry-1.docker.io` manifest for `semgrep/semgrep:1.86.0` →
`docker-content-digest: sha256:a9ea2d5621c29d815d90c2a3b2f9571da8972ef4ff855c9e4902681730240e35`),
so the tag and the digest are confirmed to name the same bytes rather than
assumed to.

The validator already preferred this shape (Issue #617) and WARNed on the bare
digest; this change makes the shipped workflow match it, and adds the
regression test that keeps it there.

Closes #640.

## Evidence

Backend/CI-only change — no web interface to screenshot.

`scripts/check-semgrep-workflow.sh` against the real workflow, after the change
(previously the third line read "pinned by digest" and a `WARN` about the
missing tag followed on stderr):

```text
OK   .github/workflows/semgrep.yml: Semgrep container image is pinned by digest and carries release tag 1.86.0 (semgrep/semgrep:<tag>@sha256:<digest>)
```

`bats tests/scripts/semgrep_workflow.bats` — 21/21 pass. The new test fails
against the pre-change workflow:

```text
not ok 17 real repository semgrep workflow carries a release tag beside the digest (issue #640)
#   `[[ "$output" == *"carries release tag"* ]]' failed
```

`./quality.sh` — full gate run after the final edit: `✅ All quality checks
passed!`

## Test Plan

- Added `tests/scripts/semgrep_workflow.bats::real repository semgrep workflow
  carries a release tag beside the digest (issue #640)` — runs the validator
  against the committed workflow and asserts it names a release tag and emits
  no missing-tag `WARN`. Observed red before the pin change, green after.
- Existing `semgrep_workflow.bats` suite (tag-only pin rejected, malformed
  digest rejected, `:latest` WARNed) unchanged and passing.
- `scripts/check-workflow-action-versions.sh` — still clean, the `uses:` pins
  in `semgrep.yml` are untouched.
