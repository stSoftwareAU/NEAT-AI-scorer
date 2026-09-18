#!/usr/bin/env bash
# Gate against an unhandled breaking neat-core bump (Issues #252, #630).
#
# scorer consumes neat-core through a git dependency PINNED to a NEAT-AI-core
# release tag (see `rust_scorer/Cargo.toml`), and `scripts/family-pins.sh`
# moves that pin onto core's newest release on every PR. So the pin arrives
# automatically and the safeguard is this gate: it fails when the pinned
# release presents a breaking bump scorer has not yet acknowledged, forcing a
# deliberate migration instead of following core blindly. This is what would
# have caught the neat-core #177 breaking type change before the build broke.
#
# Mechanism — version-baseline check:
#   * scorer records the last-handled neat-core version in the checked-in
#     `neat-core.expected-version` file.
#   * The gate reads the version actually pinned from `Cargo.lock` — the
#     `[[package]] name = "neat-core"` entry, whose `source` is the
#     `git+…?tag=v<semver>` the manifest pins. The lockfile is the resolved
#     truth: a pin the lock does not carry is not what the build compiles.
#   * The "breaking component" is the major for >= 1.0 releases and the minor
#     for pre-1.0 (0.x) releases, per SemVer. The gate FAILS when the pinned
#     release's breaking component is greater than the recorded baseline; it
#     PASSES on patch-level drift (policy) and when the two match.
#
# "Handling" a breaking bump = a deliberate scorer PR that makes the
# corresponding code change AND bumps the recorded baseline to the pinned
# neat-core version.
#
# Usage:
#   check-neat-core-version.sh [--baseline PATH] [--lockfile PATH]
#
# Defaults resolve repo-root paths: the baseline at `neat-core.expected-version`
# and the workspace `Cargo.lock`. No sibling NEAT-AI-core clone is involved —
# the pin is what the build resolves, here and on a runner.
#
# Exit codes:
#   0  versions are compatible (match, patch drift, or core behind baseline)
#   1  breaking neat-core bump above the recorded baseline — gate fails
#   2  usage / parse error (missing file, malformed version)
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-neat-core-version.sh [--baseline PATH] [--lockfile PATH]

Options:
  --baseline PATH   File recording the last-handled neat-core version
                    (default: neat-core.expected-version at the repo root).
  --lockfile PATH   Cargo.lock carrying the resolved neat-core git-tag pin
                    (default: Cargo.lock at the repo root).
  -h, --help        Show this message.

Exits 0 when compatible, 1 on an unhandled breaking bump, 2 on a usage error.
EOF
}

BASELINE=""
LOCKFILE=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --baseline)
      [[ $# -ge 2 ]] || { echo "Missing value for --baseline" >&2; usage >&2; exit 2; }
      BASELINE="$2"
      shift 2
      ;;
    --lockfile)
      [[ $# -ge 2 ]] || { echo "Missing value for --lockfile" >&2; usage >&2; exit 2; }
      LOCKFILE="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if [[ -z "$BASELINE" ]]; then
  BASELINE="$REPO_ROOT/neat-core.expected-version"
fi
if [[ -z "$LOCKFILE" ]]; then
  LOCKFILE="$REPO_ROOT/Cargo.lock"
fi

if [[ ! -f "$BASELINE" ]]; then
  echo "FAIL: baseline file not found: $BASELINE" >&2
  exit 2
fi
if [[ ! -f "$LOCKFILE" ]]; then
  echo "FAIL: lockfile not found: $LOCKFILE" >&2
  echo "      Run 'cargo fetch' to resolve the pinned neat-core release." >&2
  exit 2
fi

# First non-comment, non-blank line of the baseline file is the version.
read_baseline_version() {
  awk '
    { sub(/#.*/, "") }            # strip inline comments
    { gsub(/[[:space:]]+/, "") }  # trim all whitespace
    NF { print; exit }            # first non-empty line wins
  ' "$BASELINE"
}

# Extract the PINNED neat-core release from the lockfile: the `tag=v<semver>`
# of the `[[package]] name = "neat-core"` entry's git source. The tag is what
# the manifest pins and `scripts/family-pins.sh` moves, so it — not the
# package's own `version` line, which core may bump between releases — is the
# release this build compiles against. Only the neat-core package block counts;
# a bare scan would match any other git dependency's tag.
read_core_version() {
  awk '
    /^\[\[package\]\]/ { in_pkg = 0; next }
    /^[[:space:]]*name[[:space:]]*=[[:space:]]*"neat-core"[[:space:]]*$/ { in_pkg = 1; next }
    in_pkg && /^[[:space:]]*source[[:space:]]*=/ {
      if (match($0, /[?&]tag=v?[0-9][^"#&]*/)) {
        tag = substr($0, RSTART, RLENGTH)
        sub(/^[?&]tag=v?/, "", tag)
        print tag
        exit
      }
    }
  ' "$LOCKFILE"
}

# Validate X.Y.Z (optionally with a -prerelease/+build suffix we ignore) and
# echo the bare "major minor patch" triple. Returns non-zero when malformed.
parse_semver() {
  local raw="$1" core
  core="${raw%%[-+]*}"   # drop pre-release / build metadata
  if [[ ! "$core" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    return 1
  fi
  local major minor patch
  # Scope IFS to this single read rather than tampering with it globally.
  IFS='.' read -r major minor patch <<<"$core"
  echo "$major $minor $patch"
}

baseline_raw="$(read_baseline_version)"
if [[ -z "$baseline_raw" ]]; then
  echo "FAIL: baseline file is empty: $BASELINE" >&2
  exit 2
fi

core_raw="$(read_core_version)"
if [[ -z "$core_raw" ]]; then
  echo "FAIL: no neat-core git-tag pin found in $LOCKFILE" >&2
  echo "      Expected a [[package]] name = \"neat-core\" entry whose source is" >&2
  echo "      git+https://github.com/stSoftwareAU/NEAT-AI-core?tag=v<semver>." >&2
  exit 2
fi

if ! baseline_parts="$(parse_semver "$baseline_raw")"; then
  echo "FAIL: malformed baseline version '$baseline_raw' in $BASELINE (expected X.Y.Z)" >&2
  exit 2
fi
if ! core_parts="$(parse_semver "$core_raw")"; then
  echo "FAIL: malformed pinned neat-core version '$core_raw' in $LOCKFILE (expected vX.Y.Z)" >&2
  exit 2
fi

read -r b_major b_minor b_patch <<<"$baseline_parts"
read -r c_major c_minor c_patch <<<"$core_parts"
: "$b_patch" "$c_patch"  # patch components are informational only

remediation() {
  cat >&2 <<EOF
       neat-core has presented a breaking bump scorer has not handled.
       To clear this gate, in a single deliberate PR:
         1. Update rust_scorer for the breaking neat-core change.
         2. Bump the recorded baseline in neat-core.expected-version to $core_raw.
       See the README "neat-core breaking-bump gate" section.
EOF
}

# Breaking-bump decision (SemVer): the major signals breaking changes once a
# crate reaches 1.0; before that, the minor carries that role.
if (( c_major > b_major )); then
  echo "FAIL: breaking neat-core bump: $core_raw exceeds handled baseline $baseline_raw (major increased)" >&2
  remediation
  exit 1
fi

if (( c_major == b_major )); then
  if (( b_major == 0 )); then
    # Pre-1.0: the minor is the breaking component.
    if (( c_minor > b_minor )); then
      echo "FAIL: breaking neat-core bump: $core_raw exceeds handled baseline $baseline_raw (pre-1.0 minor increased)" >&2
      remediation
      exit 1
    fi
    if (( c_minor < b_minor )); then
      echo "OK   neat-core $core_raw is behind handled baseline $baseline_raw (no breaking bump)"
      exit 0
    fi
    echo "OK   neat-core $core_raw matches handled baseline $baseline_raw (patch-level drift allowed)"
    exit 0
  fi
  # >= 1.0: same major — minor/patch drift is additive, never breaking.
  echo "OK   neat-core $core_raw within handled baseline $baseline_raw major line (patch-level drift allowed)"
  exit 0
fi

# c_major < b_major: neat-core sits below the baseline major — not a breaking
# bump scorer needs to act on.
echo "OK   neat-core $core_raw is behind handled baseline $baseline_raw (no breaking bump)"
exit 0
