# PR Summary — Issue #565

## Summary

Added the branding banner to the root `README.md`: one image line directly
under the H1 that **hot-links** the hub's canonical per-repo preview
(`https://raw.githubusercontent.com/stSoftwareAU/NEAT-AI/Develop/docs/brand/social-previews/neat-ai-scorer.png`,
1280×640). Nothing is committed to this repo, so a future hub re-render
propagates here automatically.

A guard keeps the banner honest: `scripts/check-readme-banner.sh` (wired into
`quality.sh`, and run in CI through the existing `bats tests/scripts` job) fails
when the banner is missing, drifts away from the H1, points at a repo-local or
another repo's image, or loses alt text naming the project. No other README
change; `docs/archive/pr-summaries/README.md` stays text-only.

Closes #565.

## Evidence

No web UI to drive here — Playwright MCP is not available in this container, so
the banner was verified at the source instead:

- Hot-link reachable and correctly sized:
  `curl -sSI` → `200 image/png`; PNG IHDR decodes to **1280×640**
  (`0x0500` × `0x0280`).
- Guard script against the real README:

  ```text
  OK   README.md: carries a banner image line directly under the H1
  OK   README.md: banner hot-links the hub preview (https://raw.githubusercontent.com/stSoftwareAU/NEAT-AI/Develop/docs/brand/social-previews/neat-ai-scorer.png)
  OK   README.md: banner alt text names the project
  ```

- The banner as it renders (same hot-link the README uses):

  ![NEAT-AI-scorer banner](https://raw.githubusercontent.com/stSoftwareAU/NEAT-AI/Develop/docs/brand/social-previews/neat-ai-scorer.png)

Gate status: `./quality.sh` passed every step up to the codespell preflight,
which cannot run in this container (`codespell` is not installed and there is no
`pip`); CI runs that job for real. The full `bats tests/scripts` suite was run
separately — 615 pass, and the single failure
(`diagrams_mermaid.bats::living docs contain no box-drawing ASCII diagrams`) is
pre-existing and environmental: the container has no `en_US.UTF-8` locale, so
`grep` matches em-dash bytes against the box-drawing class. It fails identically
on a stashed (unmodified) tree.

## Test Plan

- Added `tests/scripts/readme_banner.bats` (10 cases) exercising
  `scripts/check-readme-banner.sh` against synthetic fixtures:
  - passes when the banner hot-links the hub preview under the H1;
  - fails when there is no banner, or it sits below other prose;
  - fails when it points at a repo-local image, or another repo's preview;
  - fails when alt text is empty or does not name the project;
  - shared harness contract: missing README and unknown flag rejected;
  - final case runs the validator against the **real** `README.md`, so the
    banner cannot be dropped without CI going red.
