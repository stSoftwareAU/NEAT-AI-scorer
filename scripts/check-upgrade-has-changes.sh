#!/usr/bin/env bash
# check-upgrade-has-changes.sh — Issue #90.
#
# Decides whether the weekly `cargo upgrade` workflow produced a
# meaningful dependency upgrade. Prints `changed=true` only when at
# least one `Cargo.toml` manifest in the working tree differs from
# HEAD; prints `changed=false` otherwise.
#
# Why ignore Cargo.lock? `cargo upgrade` always refreshes the lockfile
# to bring transitive deps up to date, even when every available bump
# is blocked by semver and no manifest version actually advances. The
# previous trigger condition treated a Cargo.lock-only diff as a
# meaningful upgrade, which produced noise PRs like #89 — a transitive
# `hashbrown 0.17.0 -> 0.17.1` bump and nothing else. A lockfile
# refresh of that kind is something `cargo update` will redo on the
# next CI run; it does not justify a PR titled "upgrade Cargo
# dependencies to latest compatible versions".

set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-upgrade-has-changes.sh [--repo DIR]

Prints `changed=true` if at least one `Cargo.toml` in the working tree
(root or any workspace member) has a staged or unstaged diff against
HEAD, otherwise prints `changed=false`. Differences confined to
`Cargo.lock` deliberately do not count.

Options:
  --repo DIR  Repository root to inspect (default: cwd).
  -h, --help  Show this message.

Exit codes:
  0  detection ran successfully (look at stdout for the result).
  1  the target directory is not a git repository.
  2  usage error.
EOF
}

REPO_DIR="."
while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo) REPO_DIR="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *)
      echo "Usage error: unknown option '$1'" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ ! -d "$REPO_DIR/.git" ]] && \
   ! git -C "$REPO_DIR" rev-parse --git-dir >/dev/null 2>&1; then
  echo "Error: '$REPO_DIR' is not a git repository" >&2
  exit 1
fi

# Compare against HEAD so the check sees both staged and unstaged
# manifest edits — `cargo upgrade` leaves them unstaged on the workflow
# runner, but a future variant that stages them should behave the same.
if git -C "$REPO_DIR" diff --quiet HEAD -- 'Cargo.toml' '**/Cargo.toml'; then
  echo "changed=false"
else
  echo "changed=true"
fi
