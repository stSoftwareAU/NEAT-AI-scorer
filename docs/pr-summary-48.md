## Summary

Converted the only remaining ASCII-art diagram in the living docs — the CI
job dependency graph in `README.md` — to a Mermaid `graph LR` block, so every
diagram in the repo's living documentation now uses Mermaid (the
inter-repository dependency graph in the same file was already Mermaid).
Added a bats regression suite that asserts the README's job-dependency
section is Mermaid and that no living-doc file contains box-drawing
characters, so future diagrams cannot regress to ASCII art. Closes #48.

## Evidence

CLI/docs change with no UI surface, so no screenshot is captured. The
behaviour is verified by the new bats suite which is wired into
`./quality.sh`:

```text
ok 15 README.md declares at least one Mermaid code block
ok 16 README.md CI job dependency graph is a Mermaid block
ok 17 living docs contain no box-drawing ASCII diagrams
```

`./quality.sh` passes end-to-end (fmt, clippy, cargo-deny, codespell,
shellcheck, all bats suites, cargo test, rustdoc, release build).

## Test Plan

- Added `tests/scripts/diagrams_mermaid.bats` covering:
  - README contains at least one Mermaid block.
  - The "Job dependency graph" section is rendered as a Mermaid block
    (asserted by scanning the section for a `mermaid`-fenced code block).
  - The living docs (`README.md`, `AGENTS.md`,
    `docs/performance-baseline.md`) contain no box-drawing characters.
- TDD verified: tests 16 and 17 failed against the unmodified README and
  pass after the conversion.
- Re-ran the full `./quality.sh` gate; all 82 bats tests pass and Cargo
  fmt/clippy/test/build remain clean.
