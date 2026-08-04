#!/usr/bin/env bash
# Validate that cargo CLI tools are installed from prebuilt binaries, not
# compiled from source on every CI run (Issue #208, part of #198).
#
# Why this exists:
#   Three workflows install a cargo CLI tool before using it:
#     * cargo-audit.yml — cargo-audit
#     * sbom.yml        — cargo-cyclonedx
#     * ci.yml          — cargo-deny
#   (security.yml no longer installs cargo-audit standalone: its redundant
#    in-job `cargo audit` run was removed in Issue #399, leaving the
#    `rustsec/audit-check` action as its sole advisory scan.)
#   `cargo install <tool> --locked` recompiles the tool from source on every
#   run, costing minutes of wasted CI wall-clock with no behaviour change.
#   `taiki-e/install-action` downloads a prebuilt release binary instead, so
#   the install step finishes in seconds.
#
# Rules enforced per (workflow, tool) pair:
#   1. The workflow MUST NOT compile the tool from source via
#      `cargo install <tool>` — that is the regression this issue removes.
#   2. The workflow MUST install the tool via `taiki-e/install-action` with a
#      matching `tool:` entry, so a prebuilt binary is fetched instead.
#
# Modes:
#   * `--workflow PATH --tool NAME` — validate a single file/tool pair (used
#     by BATS fixtures).
#   * no arguments — validate the four canonical (workflow, tool) pairs under
#     `.github/workflows` relative to the repo root.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/check-harness.sh
source "$SCRIPT_DIR/lib/check-harness.sh"

usage() {
  cat <<'EOF'
Usage: check-prebuilt-tool-install.sh [--workflow PATH --tool NAME] [--workflows DIR]

Options:
  --workflow PATH   Path to a single workflow YAML file to validate. Requires
                    --tool. When omitted, the four canonical workflows are
                    validated.
  --tool NAME       Cargo tool name expected to be installed as a prebuilt
                    binary (e.g. cargo-audit). Only valid with --workflow.
  --workflows DIR   Directory holding the canonical workflows (default:
                    .github/workflows relative to the repo root). The three
                    canonical pairs are cargo-audit.yml, sbom.yml and ci.yml.
  -h, --help        Show this message.

Exits 0 when every checked (workflow, tool) pair installs the tool from a
prebuilt binary and not from source. Exits non-zero otherwise.
EOF
}

WORKFLOW=""
TOOL=""
WORKFLOWS_DIR=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --workflow)
      check_flag_value --workflow $#
      WORKFLOW="$2"
      shift 2
      ;;
    --tool)
      check_flag_value --tool $#
      TOOL="$2"
      shift 2
      ;;
    --workflows)
      check_flag_value --workflows $#
      WORKFLOWS_DIR="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      check_unknown_arg "$1"
      ;;
  esac
done

# Validate a single (workflow file, tool) pair against the two rules.
check_pair() {
  local file="$1"
  local tool="$2"
  # Each ok/fail line is about this workflow file.
  local CHECK_SUBJECT="$file"

  if [[ ! -f "$file" ]]; then
    fail "workflow file not found"
    return
  fi

  # Rule 1: no source compile via `cargo install <tool>`.
  if grep -qE "cargo[[:space:]]+install[[:space:]]+${tool}([[:space:]]|$)" "$file"; then
    fail "compiles $tool from source via 'cargo install $tool' — use a prebuilt binary"
  else
    ok "no 'cargo install $tool' source compile"
  fi

  # Rule 2: prebuilt install via taiki-e/install-action with a matching tool.
  if grep -qE 'uses:[[:space:]]*taiki-e/install-action@' "$file" \
    && grep -qE "tool:[[:space:]]*${tool}([[:space:]]|,|$)" "$file"; then
    ok "installs $tool via prebuilt taiki-e/install-action"
  else
    fail "no prebuilt taiki-e/install-action step for $tool"
  fi
}

if [[ -n "$WORKFLOW" || -n "$TOOL" ]]; then
  if [[ -z "$WORKFLOW" || -z "$TOOL" ]]; then
    echo "--workflow and --tool must be supplied together" >&2
    usage >&2
    exit 2
  fi
  check_pair "$WORKFLOW" "$TOOL"
  exit "$EXIT_CODE"
fi

[[ -n "$WORKFLOWS_DIR" ]] || WORKFLOWS_DIR="$(check_repo_path ".github/workflows")"
check_require_dir "$WORKFLOWS_DIR"

# Canonical (workflow, tool) pairs. bash 3.2 has no associative arrays, so a
# parallel-indexed pair list is used.
PAIRS=(
  "cargo-audit.yml:cargo-audit"
  "sbom.yml:cargo-cyclonedx"
  "ci.yml:cargo-deny"
)

for pair in "${PAIRS[@]}"; do
  wf="${pair%%:*}"
  tool="${pair#*:}"
  check_pair "$WORKFLOWS_DIR/$wf" "$tool"
done

exit "$EXIT_CODE"
