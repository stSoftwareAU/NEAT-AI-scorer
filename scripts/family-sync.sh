#!/usr/bin/env bash
# Refresh this repo's copies of the canonical NEAT-AI family scripts
# (Issues #629, #630).
#
# Each synced script has a single home — `scripts/<name>` on NEAT-AI-core
# `Develop` — and every Rust sibling carries a byte-identical copy. This
# script fetches those copies and writes them here when they differ; behaviour
# changes are made in NEAT-AI-core and re-copied outward, never edited
# downstream. The `family-sync` CI job runs it on every pull request.
#
# Synced files:
#   runlib.sh       build-once / install-once CLI installer (Issue #629).
#   family-pins.sh  moves this repo's NEAT-AI family git-tag pins to the
#                   latest release (Issue #630).
#
# Exit status:
#   0  every copy already matched, or was refreshed (`--check`: all matched).
#   1  at least one copy is stale and `--check` was given.
#   2  a fetch failed, or a fetched file is not the script it claims to be —
#      the copy on disk is left untouched. A fetch that cannot be trusted is
#      never a sync.
#
# Cross-platform: macOS bash 3.2, Ubuntu, AWS Linux.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

CANONICAL_BASE="https://raw.githubusercontent.com/stSoftwareAU/NEAT-AI-core/Develop/scripts"

# The canonical scripts, and the declaration each fetched file must contain to
# be recognised as that script. bash 3.2 has no associative arrays, so the two
# are parallel arrays walked by index.
CANONICAL_NAMES=("runlib.sh" "family-pins.sh")
CANONICAL_SENTINELS=("runlib_install" "family-pins")

SELECTED=""
TARGET=""
SOURCE=""
CHECK_ONLY=0

usage() {
  cat <<'EOF'
Usage: family-sync.sh [--check] [--file NAME] [--source PATH] [--target PATH]

Options:
  --check         Report a stale copy (exit 1) instead of refreshing it.
  --file NAME     Sync only this canonical script (runlib.sh, family-pins.sh)
                  instead of every one of them.
  --source PATH   Read the canonical copy from PATH instead of fetching it
                  from NEAT-AI-core Develop (used by the tests). Implies a
                  single-file run.
  --target PATH   Path of the local copy to refresh (default: scripts/<NAME>
                  relative to the repo root). The file name decides which
                  canonical script it is compared against, so this also
                  implies a single-file run.
  -h, --help      Show this message.

Exits 0 when every local copy matches the canonical one (or was refreshed to
match it), 1 when --check finds one stale, and 2 when a canonical copy could
not be fetched or is not the script it claims to be.
EOF
}

# Index of canonical script $1 in CANONICAL_NAMES, or non-zero when unknown.
canonical_index() {
  local name="$1" i
  for ((i = 0; i < ${#CANONICAL_NAMES[@]}; i++)); do
    if [[ "${CANONICAL_NAMES[i]}" == "$name" ]]; then
      printf '%s\n' "$i"
      return 0
    fi
  done
  return 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --check)
      CHECK_ONLY=1
      shift
      ;;
    --file)
      [[ $# -ge 2 ]] || { echo "--file needs a name" >&2; exit 2; }
      SELECTED="$2"
      shift 2
      ;;
    --source)
      [[ $# -ge 2 ]] || { echo "--source needs a path" >&2; exit 2; }
      SOURCE="$2"
      shift 2
      ;;
    --target)
      [[ $# -ge 2 ]] || { echo "--target needs a path" >&2; exit 2; }
      TARGET="$2"
      shift 2
      ;;
    -h | --help)
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

# A --target names the script by its own file name, so `--target /tmp/x/runlib.sh`
# is compared against the canonical runlib.sh. A --source without either is a
# single-file run over the default target.
if [[ -z "$SELECTED" && -n "$TARGET" ]]; then
  # Pure-bash basename: this runs before the curl precondition, so it must not
  # depend on a tool a stripped PATH may not carry.
  SELECTED="${TARGET##*/}"
fi
if [[ -z "$SELECTED" && -n "$SOURCE" ]]; then
  echo "family-sync: --source needs --file NAME or --target PATH so the script being synced is known" >&2
  exit 2
fi

SYNC_NAMES=()
if [[ -n "$SELECTED" ]]; then
  if ! canonical_index "$SELECTED" >/dev/null; then
    echo "family-sync: '${SELECTED}' is not a canonical family script (${CANONICAL_NAMES[*]})" >&2
    exit 2
  fi
  SYNC_NAMES=("$SELECTED")
else
  SYNC_NAMES=("${CANONICAL_NAMES[@]}")
fi

WORK_DIR="$(mktemp -d)" || {
  echo "family-sync: could not create a temporary directory" >&2
  exit 2
}
trap 'rm -rf "${WORK_DIR}"' EXIT

# Refresh (or, under --check, verify) canonical script $1. Returns 0 when the
# copy matches or was refreshed, 1 when --check found it stale, and 2 on a
# fetch or validation failure.
sync_one() {
  local name="$1" target="$2" source="$3"
  local index sentinel url fetched
  index="$(canonical_index "$name")" || return 2
  sentinel="${CANONICAL_SENTINELS[index]}"
  url="${CANONICAL_BASE}/${name}"
  fetched="${WORK_DIR}/${name}"

  if [[ -n "${source}" ]]; then
    if [[ ! -f "${source}" ]]; then
      echo "family-sync: canonical source not found: ${source}" >&2
      return 2
    fi
    cp "${source}" "${fetched}"
  else
    if ! command -v curl >/dev/null 2>&1; then
      echo "family-sync: curl not found — it is what fetches ${url}" >&2
      return 2
    fi
    if ! curl --proto "=https" --tlsv1.2 -sSfL --retry 3 --retry-delay 2 \
      --connect-timeout 30 -o "${fetched}" "${url}"; then
      echo "family-sync: could not fetch ${url}" >&2
      return 2
    fi
  fi

  # A fetch that returned an error page, a redirect body or an empty file is a
  # failed fetch, not a new canonical copy: never overwrite a working script
  # with it.
  if [[ ! -s "${fetched}" ]]; then
    echo "family-sync: fetched an empty file from ${url}" >&2
    return 2
  fi
  if [[ "$(head -n 1 "${fetched}")" != "#!/usr/bin/env bash" ]]; then
    echo "family-sync: fetched file is not a bash script — refusing to install it over ${target}" >&2
    return 2
  fi
  if ! grep -q "${sentinel}" "${fetched}"; then
    echo "family-sync: fetched file declares no ${sentinel} — refusing to install it over ${target}" >&2
    return 2
  fi

  if [[ -f "${target}" ]] && cmp -s "${fetched}" "${target}"; then
    echo "family-sync: ${target} matches NEAT-AI-core Develop"
    return 0
  fi

  if [[ "${CHECK_ONLY}" -eq 1 ]]; then
    echo "family-sync: ${target} differs from NEAT-AI-core Develop — run ./scripts/family-sync.sh" >&2
    return 1
  fi

  mkdir -p "$(dirname "${target}")"
  cp "${fetched}" "${target}"
  chmod +x "${target}"
  echo "family-sync: refreshed ${target} from NEAT-AI-core Develop"
  return 0
}

# Every script is visited even after one of them is found stale or unfetchable,
# so one run reports every problem; the worst status seen is the exit status.
STATUS=0
for name in "${SYNC_NAMES[@]}"; do
  target="${TARGET}"
  [[ -n "$target" ]] || target="${REPO_ROOT}/scripts/${name}"
  one_status=0
  sync_one "$name" "$target" "$SOURCE" || one_status=$?
  if ((one_status > STATUS)); then
    STATUS="$one_status"
  fi
done

exit "$STATUS"
