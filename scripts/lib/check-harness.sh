#!/usr/bin/env bash
# Shared CLI harness for the `scripts/check-*.sh` guard scripts (Issue #512).
#
# This file is *sourced*, never executed. It owns the contract every validator
# shares — the `--FLAG PATH` / `-h` argument loop, default-target resolution
# relative to the repo root, the "file not found" guard, and the
# accumulate-and-report protocol (`OK   `/`FAIL ` prefixes, failures on stderr,
# `EXIT_CODE=1` without aborting the run). The per-script rule checks — the part
# that genuinely differs — stay in the individual scripts.
#
# Contract for a sourcing script:
#   * define `usage()` before calling `parse_check_args` (the harness prints it
#     for `-h/--help` and for an unknown flag);
#   * set `CHECK_SUBJECT` to the file each `ok`/`fail` line is about, or leave
#     it empty when the message names its own subject;
#   * end with `exit "$EXIT_CODE"`.
#
# Changing the CLI contract — a new flag, machine-readable output, a different
# exit-code convention — is a single edit here rather than ~30 copy-pasted ones.

# EXIT_CODE, CHECK_* and the functions below are consumed by the sourcing
# script, so ShellCheck cannot see their use from inside this file.
# shellcheck disable=SC2034

# Absolute path to the repository root (the parent of `scripts/`).
CHECK_REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Subject prefixed to every `ok`/`fail` line. Empty means "no subject prefix".
CHECK_SUBJECT=""

# Accumulated status: flipped to 1 by `fail`, so a run reports every violation
# rather than aborting on the first one.
EXIT_CODE=0

# Target resolved by `parse_check_args`.
CHECK_TARGET=""

# check_die <exit-code> <message...> — report on stderr and stop.
check_die() {
  local code="$1"
  shift
  echo "$*" >&2
  exit "$code"
}

# check_unknown_arg <arg> — the shared unknown-flag contract: name the offending
# argument, print the usage block on stderr, exit 2.
check_unknown_arg() {
  echo "Unknown argument: $1" >&2
  usage >&2
  exit 2
}

# check_flag_value <flag> <remaining-arg-count> — guard a flag that needs a
# value, so a bare `--workflow` fails with a message instead of an unbound
# variable error under `set -u`.
check_flag_value() {
  [[ "$2" -ge 2 ]] || check_die 2 "$1 requires a PATH argument"
}

# check_repo_path <relative-path> — absolute path to a repo-relative file.
check_repo_path() {
  echo "$CHECK_REPO_ROOT/$1"
}

# check_require_file <path> [label] — the shared "not found" guard.
check_require_file() {
  [[ -f "$1" ]] || check_die 2 "${2:-Workflow file} not found: $1"
}

# check_require_dir <path> [label] — directory flavour of the guard above.
check_require_dir() {
  [[ -d "$1" ]] || check_die 2 "${2:-Workflows directory} not found: $1"
}

# parse_check_args <flag> <default-relative-path> "$@" — the single-target
# argument loop shared by most validators. Sets `CHECK_TARGET` to the flag's
# value, or to `<repo root>/<default-relative-path>` when the flag is absent.
# Pass an empty default when the caller resolves its own fallback.
parse_check_args() {
  local flag="$1" default="$2"
  shift 2
  CHECK_TARGET=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      "$flag")
        check_flag_value "$flag" $#
        CHECK_TARGET="$2"
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
  if [[ -z "$CHECK_TARGET" && -n "$default" ]]; then
    CHECK_TARGET="$(check_repo_path "$default")"
  fi
}

# ok <message...> — record a satisfied rule on stdout.
ok() {
  echo "OK   ${CHECK_SUBJECT:+$CHECK_SUBJECT: }$*"
}

# fail <message...> — record a violated rule on stderr and remember the failure.
fail() {
  echo "FAIL ${CHECK_SUBJECT:+$CHECK_SUBJECT: }$*" >&2
  EXIT_CODE=1
}

# check_subject <subject> — set the file each subsequent `ok`/`fail` line is
# about. Pass an empty string to drop the prefix.
check_subject() {
  CHECK_SUBJECT="$1"
}
