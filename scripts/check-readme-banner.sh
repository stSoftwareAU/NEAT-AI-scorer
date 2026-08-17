#!/usr/bin/env bash
# check-readme-banner.sh — Issue #565.
#
# Guards the README branding banner. The banner is a single image line directly
# under the H1 that **hot-links** the hub's canonical per-repo preview
# (`stSoftwareAU/NEAT-AI` `docs/brand/social-previews/neat-ai-scorer.png`), so a
# hub re-render propagates here automatically and this repo commits no image.
#
# The gate enforces:
#   1. The first non-blank line after the H1 is a markdown image — the banner.
#   2. Its target is the hub raw hot-link for *this* repo's preview, not a
#      repo-local path and not another repo's art.
#   3. Its alt text is non-empty and names the project.
#
# Usage:
#   check-readme-banner.sh [--readme PATH]
#
# Exit codes:
#   0  the banner is present, hot-linked and described
#   1  the banner is missing, local, mis-targeted or undescribed
#   2  usage error

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/check-harness.sh
source "$SCRIPT_DIR/lib/check-harness.sh"

# The hub's canonical preview for this repo. Any ref on the hub is accepted so a
# branch rename does not break the gate; the file name is pinned.
BANNER_HOST="https://raw.githubusercontent.com/stSoftwareAU/NEAT-AI/"
BANNER_FILE="docs/brand/social-previews/neat-ai-scorer.png"
PROJECT_NAME="NEAT-AI-scorer"

usage() {
  cat <<'EOF'
Usage: check-readme-banner.sh [--readme PATH]

Options:
  --readme PATH  README to validate (default: README.md at the repo root).
  -h, --help     Show this message.

Exits 0 when the README carries the hot-linked branding banner under its H1.
EOF
}

parse_check_args --readme "README.md" "$@"
README="$CHECK_TARGET"
check_require_file "$README" "README"
check_subject "${README##*/}"

# The first non-blank line after the H1 — the banner's only allowed home.
banner_line="$(awk '
  /^# / { seen = 1; next }
  seen && NF { print; exit }
' "$README")"

if [[ "$banner_line" != !\[* ]]; then
  fail "no branding banner directly under the H1 — add one image line hot-linking ${BANNER_HOST}<ref>/${BANNER_FILE} (Issue #565)"
  exit "$EXIT_CODE"
fi
ok "carries a banner image line directly under the H1"

alt="${banner_line#!\[}"
alt="${alt%%]*}"
url="${banner_line#*](}"
url="${url%%)*}"

# 2. Hot-linked to the hub's preview for this repo.
if [[ "$url" == "$BANNER_HOST"*"/$BANNER_FILE" ]]; then
  ok "banner hot-links the hub preview ($url)"
else
  fail "banner target '$url' is not the hub hot-link — expected ${BANNER_HOST}<ref>/${BANNER_FILE} so a hub re-render propagates and no image is committed here (Issue #565)"
fi

# 3. Alt text present and naming the project.
if [[ -z "${alt//[[:space:]]/}" ]]; then
  fail "banner alt text is empty — describe the image and name $PROJECT_NAME (Issue #565)"
elif grep -qiF "$PROJECT_NAME" <<<"$alt"; then
  ok "banner alt text names the project"
else
  fail "banner alt text '$alt' does not name $PROJECT_NAME (Issue #565)"
fi

exit "$EXIT_CODE"
