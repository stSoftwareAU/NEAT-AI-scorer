#!/usr/bin/env bash
# Auto-format helper for the CI PR job (Issue #19).
#
# Responsibilities:
#   * Emit the deterministic commit message the workflow uses when pushing
#     rustfmt-generated fixes back onto a PR branch.
#   * Detect whether the working tree has pending changes after `cargo fmt`
#     ran, so the workflow can commit only when needed (idempotent guard).
#
# Running cargo is the workflow's job — this script does not invoke any
# build tooling, which keeps the BATS suite hermetic (no Rust toolchain
# required in the bash helper tests).
set -euo pipefail

COMMIT_MESSAGE="chore(fmt): apply cargo fmt fixes

Automated by the auto-format PR job — see issue #19."

usage() {
  cat <<'EOF'
Usage: auto-format.sh <mode> [--repo DIR]

Modes (exactly one):
  --commit-message   Print the deterministic commit message.
  --check-changes    Exit 0 when the git working tree has pending changes
                     (tracked or untracked), exit 1 when it is clean.

Options:
  --repo DIR         Repository directory for git operations (default: cwd).
  -h, --help         Show this message.
EOF
}

MODE=""
REPO_DIR="."

set_mode() {
  if [[ -n "$MODE" ]]; then
    echo "Usage error: only one mode may be supplied" >&2
    usage >&2
    exit 2
  fi
  MODE="$1"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --commit-message) set_mode commit-message; shift ;;
    --check-changes)  set_mode check-changes;  shift ;;
    --repo)           REPO_DIR="$2"; shift 2 ;;
    -h|--help)        usage; exit 0 ;;
    *)
      echo "Usage error: unknown option '$1'" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$MODE" ]]; then
  echo "Usage error: a mode is required" >&2
  usage >&2
  exit 2
fi

case "$MODE" in
  commit-message)
    printf '%s\n' "$COMMIT_MESSAGE"
    ;;

  check-changes)
    # Inspect both tracked modifications and untracked files. `git status
    # --porcelain` prints one line per changed path and nothing for a clean
    # tree, which is the cheapest way to cover both cases in a single call.
    status_output="$(cd "$REPO_DIR" && git status --porcelain)"
    if [[ -z "$status_output" ]]; then
      echo "clean: no formatting changes detected"
      exit 1
    else
      echo "dirty: formatting changes detected"
      exit 0
    fi
    ;;
esac
