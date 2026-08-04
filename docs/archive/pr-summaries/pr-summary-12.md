## Summary
Added a `## Related Repositories` section to `README.md` listing all 7 public NEAT-AI-* repositories with one-line descriptions, links, and a Mermaid dependency diagram. Closes #12.

The section follows the acceptance criteria from the canonical block defined in stSoftwareAU/NEAT-AI-core#18: all 7 public repos are linked, each has a one-sentence role description, and the Mermaid graph shows dependency direction (path deps, Deno FFI invocation, snapshot reads, etc.). The existing "Source" table was left untouched — the new section is broader and complementary.

## Evidence
README-only documentation change; no UI or runtime behaviour affected. `./quality.sh` passes cleanly (shellcheck, cargo-deny, fmt, clippy `-D warnings`, workspace tests 23+2 passed, rustdoc, release build).

## Test Plan
- `./quality.sh` — full local gate passes after the change.
- No new tests required: README-only change with no code paths touched.
