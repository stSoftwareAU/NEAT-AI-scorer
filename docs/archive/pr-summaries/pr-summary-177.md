# PR Summary — Add `SECURITY.md` vulnerability disclosure policy (Issue #177)

## Summary

The repository had no security policy at any of the three GitHub-recognised
paths (`SECURITY.md`, `.github/SECURITY.md`, `docs/SECURITY.md`), so a reporter
had no private channel and would default to opening a public issue —
disclosing a vulnerability before a fix exists.

This PR adds a root `SECURITY.md` declaring:

- a **private reporting route** — GitHub private vulnerability reporting
  (preferred) plus a `security@stsoftware.com.au` contact email;
- **response targets** — acknowledgement within 3 business days, triage within
  10;
- a **supported-versions table** — fixes land on the active `Develop` branch
  (the tool ships no semantic-version releases; `neat-core` is a local `path`
  dependency).

To keep the policy from silently rotting, it mirrors the CODEOWNERS guard
(Issue #176): a `scripts/check-security-policy.sh` validator enforces the file
exists at a recognised path, is non-empty, and contains a private reporting
route, a response-time expectation, and a supported-versions table. The
validator is wired into `quality.sh` and `SECURITY.md` is added to the CI
`validation` job's required-files list.

Closes #177.

## Evidence

This is a documentation / repo-hygiene change with no web interface, so no
screenshot applies. Verification is via the shell validator and its BATS
suite.

Validator against the real `SECURITY.md`:

```text
OK   SECURITY.md: is non-empty
OK   SECURITY.md: declares a private reporting route (GitHub private reporting and/or email)
OK   SECURITY.md: states an expected acknowledgement / response time
OK   SECURITY.md: includes a supported-versions table
```

`markdownlint-cli2 SECURITY.md` → `0 error(s)`; `codespell` → no typos.

```mermaid
flowchart LR
    R[Reporter finds a vuln] -->|private advisory| G[GitHub Security tab]
    R -->|email| E[security@stsoftware.com.au]
    G --> M[Maintainers triage]
    E --> M
    M -->|ack <= 3 business days| A[Acknowledge]
    A --> F[Fix on Develop]
    F --> D[Coordinated disclosure]
```

## Test Plan

- Added `tests/scripts/security_policy.bats` (10 cases) exercising
  `scripts/check-security-policy.sh` end-to-end against temporary fixtures:
  - passes on a canonical policy and on an email-only reporting route;
  - fails when the private reporting route, response-time expectation, or
    supported-versions table is missing (including a heading present but no
    table);
  - fails on an empty file and a non-existent path;
  - unknown flag prints usage and exits non-zero;
  - the real repository `SECURITY.md` satisfies every rule.
- `shellcheck -s bash scripts/check-security-policy.sh` — clean.
- Full `bats tests/scripts` suite — all pass except the pre-existing
  `cargo metadata` test, which fails only because the `NEAT-AI-core` sibling
  `path` dependency is not checked out in this local environment (CI checks it
  out). That failure is unrelated to this change, which touches no Rust code.
