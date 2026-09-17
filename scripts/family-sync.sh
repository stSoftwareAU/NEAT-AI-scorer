#!/usr/bin/env bash
# Refresh scripts/runlib.sh from its one canonical home (Issue #629).
#
# `scripts/runlib.sh` has a single home — `scripts/runlib.sh` on NEAT-AI-core
# `Develop` — and every Rust sibling carries a byte-identical copy. This
# script fetches that copy and writes it here when the two differ; behaviour
# changes are made in NEAT-AI-core and re-copied outward, never edited
# downstream. The `family-sync` CI job runs it on every pull request.
#
# Exit status:
#   0  the copy already matched, or it was refreshed (`--check`: matched).
#   1  the copy is stale and `--check` was given.
#   2  the fetch failed, or the fetched file is not a runlib.sh — the copy on
#      disk is left untouched. A fetch that cannot be trusted is never a sync.
#
# Cross-platform: macOS bash 3.2, Ubuntu, AWS Linux.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

CANONICAL_URL="https://raw.githubusercontent.com/stSoftwareAU/NEAT-AI-core/Develop/scripts/runlib.sh"
TARGET="${REPO_ROOT}/scripts/runlib.sh"
SOURCE=""
CHECK_ONLY=0

usage() {
  cat <<'EOF'
Usage: family-sync.sh [--check] [--source PATH] [--target PATH]

Options:
  --check         Report a stale copy (exit 1) instead of refreshing it.
  --source PATH   Read the canonical copy from PATH instead of fetching it
                  from NEAT-AI-core Develop (used by the tests).
  --target PATH   Path of the local copy to refresh (default:
                  scripts/runlib.sh relative to the repo root).
  -h, --help      Show this message.

Exits 0 when the local copy matches the canonical one (or was refreshed to
match it), 1 when --check finds it stale, and 2 when the canonical copy could
not be fetched or is not a runlib.sh.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --check)
      CHECK_ONLY=1
      shift
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

WORK_DIR="$(mktemp -d)" || {
  echo "family-sync: could not create a temporary directory" >&2
  exit 2
}
trap 'rm -rf "${WORK_DIR}"' EXIT
FETCHED="${WORK_DIR}/runlib.sh"

if [[ -n "${SOURCE}" ]]; then
  if [[ ! -f "${SOURCE}" ]]; then
    echo "family-sync: canonical source not found: ${SOURCE}" >&2
    exit 2
  fi
  cp "${SOURCE}" "${FETCHED}"
else
  if ! command -v curl >/dev/null 2>&1; then
    echo "family-sync: curl not found — it is what fetches ${CANONICAL_URL}" >&2
    exit 2
  fi
  if ! curl --proto "=https" --tlsv1.2 -sSfL --retry 3 --retry-delay 2 \
    --connect-timeout 30 -o "${FETCHED}" "${CANONICAL_URL}"; then
    echo "family-sync: could not fetch ${CANONICAL_URL}" >&2
    exit 2
  fi
fi

# A fetch that returned an error page, a redirect body or an empty file is a
# failed fetch, not a new canonical copy: never overwrite a working script
# with it.
if [[ ! -s "${FETCHED}" ]]; then
  echo "family-sync: fetched an empty file from ${CANONICAL_URL}" >&2
  exit 2
fi
if [[ "$(head -n 1 "${FETCHED}")" != "#!/usr/bin/env bash" ]]; then
  echo "family-sync: fetched file is not a bash script — refusing to install it over ${TARGET}" >&2
  exit 2
fi
if ! grep -q 'runlib_install' "${FETCHED}"; then
  echo "family-sync: fetched file declares no runlib_install — refusing to install it over ${TARGET}" >&2
  exit 2
fi

if [[ -f "${TARGET}" ]] && cmp -s "${FETCHED}" "${TARGET}"; then
  echo "family-sync: ${TARGET} matches NEAT-AI-core Develop"
  exit 0
fi

if [[ "${CHECK_ONLY}" -eq 1 ]]; then
  echo "family-sync: ${TARGET} differs from NEAT-AI-core Develop — run ./scripts/family-sync.sh" >&2
  exit 1
fi

mkdir -p "$(dirname "${TARGET}")"
cp "${FETCHED}" "${TARGET}"
chmod +x "${TARGET}"
echo "family-sync: refreshed ${TARGET} from NEAT-AI-core Develop"
