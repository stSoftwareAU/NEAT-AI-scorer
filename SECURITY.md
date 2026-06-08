# Security Policy

This document describes how to report a security vulnerability in
**NEAT-AI-scorer** and what to expect once you do. It follows
[GitHub's guidance on adding a security policy](https://docs.github.com/en/code-security/getting-started/adding-a-security-policy-to-your-repository),
so GitHub surfaces it in the repository's **Security** tab.

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.** A public
issue discloses the problem before a fix exists and puts every consumer of the
tool at risk.

Use one of the private channels below instead:

1. **GitHub private vulnerability reporting (preferred).** Open the
   repository's **Security** tab and choose **Report a vulnerability** to start
   a private advisory visible only to you and the maintainers. See
   [Privately reporting a security vulnerability](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability).
2. **Email.** If you cannot use GitHub private reporting, email
   **security@stsoftware.com.au** with the details. Use the subject line
   `NEAT-AI-scorer security report`.

Whichever channel you choose, please include as much of the following as you
can so we can reproduce and triage quickly:

- a description of the vulnerability and its impact;
- the affected component, file, or command (for example the `rust_scorer`
  CLI);
- step-by-step reproduction instructions, including a minimal `creature.json`
  and data directory where relevant;
- any proof-of-concept input, logs, or stack traces;
- the commit SHA or branch you tested against.

## Response targets

We aim to honour the following timeline. These are targets, not contractual
guarantees, and they are measured in business days.

| Stage                          | Target                              |
| ------------------------------ | ----------------------------------- |
| Acknowledge your report        | Within 3 business days              |
| Initial assessment / triage    | Within 10 business days             |
| Fix or mitigation plan         | Communicated after triage           |
| Public disclosure              | Coordinated with you once a fix lands |

We will keep you informed of progress and coordinate the timing of any public
disclosure with you. Please give us a reasonable opportunity to remediate
before disclosing publicly.

## Emergency dependency bump

When a dependency advisory needs an urgent, out-of-band fix — a malicious crate
version, a leaked maintainer token, or an actively-exploited CVE in a transitive
dependency — you do **not** have to wait for the normal per-PR bump cadence or
the 24-hour quarantine window. Use the existing tooling:

1. **Bypass the quarantine window.** Run
   `./bump-deps.sh --quarantine-hours 0` to pull the fixed crate version
   immediately (the default 24-hour `--quarantine-hours` gate is what would
   otherwise defer a just-published version). Add `--skip-external` if you only
   need to refresh the internal `neat-core` pin, or `--skip-internal` to bump
   crates.io alone.
2. **Confirm the tree is clean.** `bump-deps.sh` runs `cargo audit` and a
   release build as part of the bump; make sure both pass (it exits non-zero
   if the bump produces a non-passing tree). Run `./quality.sh` for the full
   local gate before opening the PR.
3. **Open an expedited PR.** Reference the advisory and flag it for expedited
   review so a maintainer can merge ahead of the normal queue.

This path simply writes down the controls already in the repository so a
responder does not have to reverse-engineer `bump-deps.sh` flags mid-incident.

## Supported versions

NEAT-AI-scorer is developed as a single-consumer internal tool and is not
published to a registry (`neat-core` is consumed as a local `path`
dependency). There is no semantic-version release line, so security fixes are
applied to the active development branch only.

| Version            | Supported          |
| ------------------ | ------------------ |
| `Develop` (latest) | :white_check_mark: |
| Older commits      | :x:                |

Always update to the latest commit on `Develop` to receive security fixes.

## Scope

This policy covers the code in this repository. Vulnerabilities in the
upstream [`NEAT-AI-core`](https://github.com/stSoftwareAU/NEAT-AI-core)
dependency should be reported against that repository.
