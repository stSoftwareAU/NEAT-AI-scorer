#!/bin/bash
# Local gate — mirrors `.github/workflows/ci.yml` plus shellcheck, cargo-deny, doc, release.
set -euo pipefail

if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi

export RUSTFLAGS="-D warnings"
echo "🔍 Pre-deployment Quality Check (NEAT-AI-scorer)"
echo "==============================================="

echo "📝 Checking bash script syntax..."
find . -name "*.sh" -type f -not -path "./target/*" -not -path "./.git/*" -exec bash -n {} \;

echo "Running shellcheck on bash scripts..."
if ! command -v shellcheck &>/dev/null; then
  echo "shellcheck is required — install: https://github.com/koalaman/shellcheck#installing"
  exit 1
fi
SHELLCHECK_FAILED=0
while IFS= read -r script; do
  echo "  shellcheck: $script"
  if ! shellcheck -s bash "$script"; then
    SHELLCHECK_FAILED=1
  fi
done < <(find . -name "*.sh" -type f -not -path "./target/*" -not -path "./.git/*")
if [[ "$SHELLCHECK_FAILED" -ne 0 ]]; then
  echo "shellcheck: FAILED"
  exit 1
fi
echo "shellcheck: all scripts passed"

echo "🦀 Validating pinned rust-toolchain.toml (Issue #209)..."
./scripts/check-rust-toolchain.sh

echo "🛡️  Validating crate-level rustc lint hardening (Issue #274)..."
./scripts/check-rust-lints.sh

echo "🔗 Validating NEAT-AI-core checkout path strategy in workflows..."
./scripts/check-workflow-paths.sh

echo "🚧 Gating on unhandled breaking neat-core bump (Issue #252)..."
# Mirrors the CI `validation` job step. Runs only when the sibling neat-core
# clone is present (CI always has it; local checkouts may not), so a missing
# sibling does not block an otherwise valid local gate run.
if [ -f "./../NEAT-AI-core/Cargo.toml" ]; then
  ./scripts/check-neat-core-version.sh
else
  echo "   ⚠️  sibling ../NEAT-AI-core not found — skipping (CI runs this for real)"
fi

echo "🔐 Validating Gitleaks workflow hardening (Issue #21)..."
./scripts/check-gitleaks-workflow.sh

echo "🛡️  Validating Semgrep SAST workflow (Issue #47)..."
./scripts/check-semgrep-workflow.sh

echo "🦀 Validating Cargo Security Audit workflow (Issue #64)..."
./scripts/check-cargo-audit-workflow.sh

echo "📋 Validating SBOM workflow (Issue #172)..."
./scripts/check-sbom-workflow.sh

echo "⚡ Validating prebuilt cargo tool installs in CI (Issue #208)..."
./scripts/check-prebuilt-tool-install.sh

echo "🧹 Validating Cargo Quality (fmt + clippy) workflow (Issue #66)..."
./scripts/check-cargo-quality-workflow.sh

echo "🐚 Guarding against duplicated ShellCheck across workflows (Issue #157)..."
./scripts/check-shellcheck-dedup.sh

echo "📝 Validating Markdown Lint workflow (Issue #63)..."
./scripts/check-markdown-lint-workflow.sh

echo "🎬 Validating Actionlint workflow (Issue #195)..."
./scripts/check-actionlint-workflow.sh

echo "📦 Validating Dependency Review workflow (Issue #62)..."
./scripts/check-dependency-review-workflow.sh

echo "👥 Validating CODEOWNERS coverage for .github/workflows/ (Issue #176)..."
./scripts/check-codeowners.sh

echo "🔒 Validating SECURITY.md vulnerability disclosure policy (Issue #177)..."
./scripts/check-security-policy.sh

echo "🪄 Validating auto-format PR workflow (Issue #19)..."
./scripts/check-auto-format-workflow.sh

echo "🧭 Validating CI job dependency graph (Issue #23)..."
./scripts/check-ci-job-graph.sh

echo "🔢 Validating workflow action versions for Node 24 compat (Issue #24)..."
./scripts/check-workflow-action-versions.sh

echo "🔑 Validating CI least-privilege token scope (Issue #155)..."
./scripts/check-ci-permissions.sh

echo "🔐 Validating security.yml checkouts disable credential persistence (Issue #388)..."
./scripts/check-persist-credentials.sh

echo "⏱️  Validating per-job timeout-minutes across workflows (Issue #154)..."
./scripts/check-workflow-timeouts.sh

echo "🔁 Validating concurrency groups on pile-up-prone workflows (Issue #156)..."
./scripts/check-workflow-concurrency.sh

echo "🔒 Validating credential-free checkouts set persist-credentials: false (Issue #380)..."
./scripts/check-persist-credentials.sh

echo "📖 Validating README 'matches CI' block aligns with the CI quality job (Issue #212)..."
./scripts/check-readme-ci-alignment.sh

echo "📝 Running codespell preflight (mirrors CI spell-check job)..."
if ! ./scripts/spell-check.sh; then
  echo "spell-check: FAILED — fix the typos above or update .codespellrc (see README)."
  exit 1
fi

echo "🧰 Running bash helper tests (bats)..."
if command -v bats &>/dev/null; then
  # Only execute the bats suites we ship under tests/scripts — keeps runtime
  # scoped to shell helpers and avoids pulling in unrelated directories.
  if [ -d "tests/scripts" ]; then
    bats tests/scripts
  fi
else
  echo "⚠️  bats not installed — skipping shell helper tests"
  echo "   Install with: brew install bats-core  (or your package manager)"
fi

echo "📦 Dependency upgrade step (opt-in — read-only by default, Issue #210)..."
# Default gate is read-only against Cargo.lock / Cargo.toml. Pass --upgrade
# (or QUALITY_UPGRADE=1) to bump library dependencies via cargo-edit.
./scripts/cargo-upgrade.sh "$@"

echo "📜 Running licence and dependency audit (cargo-deny)..."
if ! command -v cargo-deny &>/dev/null; then
  echo "cargo-deny is required — install: cargo install cargo-deny --locked"
  exit 1
fi
cargo deny check

echo "🪄 Auto-formatting Rust code..."
cargo fmt --all

echo "🔧 Running linter..."
cargo clippy --workspace --all-targets --all-features -- \
  -D warnings \
  -D clippy::filter_next \
  -D clippy::collapsible_if

echo "✅ Running type checks..."
cargo check --workspace --all-targets --all-features

echo "🏗️ Building (debug)..."
cargo build --workspace

echo "🧪 Running tests..."
cargo test --workspace --all-features -- --test-threads=2

echo "📖 Building documentation..."
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

echo "🏗️ Building release..."
cargo build --workspace --release

echo "✅ All quality checks passed!"
