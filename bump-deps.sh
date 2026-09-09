#!/usr/bin/env bash
# bump-deps.sh — refresh Cargo dependencies before quality.sh (Issue #55).
#
# Invoked by the automation worker before quality.sh, per the standing
# dependency-bump contract:
#
#   1. Internal: NEAT-AI-core pin — bump to upstream Develop HEAD if a
#      `rev = "..."` pin exists in any workspace member's Cargo.toml. If
#      neat-core is resolved via a `path = "..."` sibling clone (as in this
#      repo by default — see AGENTS.md), there is no SHA to advance and the
#      step is a no-op.
#   2. External: crates.io — `cargo update`, honouring the quarantine window
#      (`--quarantine-hours`, default `$VIBE_BUMP_QUARANTINE_HOURS` / 24h)
#      so versions published less than N hours ago are deferred.
#   3. `cargo audit` — fails non-zero on any reported advisory, naming the
#      offending crate + advisory ID. A cargo-audit that is not installed is
#      a tooling gap, not a bump rejection (Issue #619): the stage is skipped
#      loudly and the advisory scan is left to the CI job that owns it
#      (`.github/workflows/cargo-audit.yml` runs it on every PR). Pass
#      `--require-audit` (or set `BUMP_DEPS_REQUIRE_AUDIT=1`) to demand it.
#   4. `cargo build --release` — confirms the bumped tree compiles.
#
# Exit 0 = clean (or no-op). Non-zero = bump rejected by audit/build/etc.;
# the worker reverts per the contract.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: bump-deps.sh [options]

Refreshes Cargo dependencies and the NEAT-AI-core pin, then runs cargo
audit and a release build. Prints a one-line summary.

Options:
  --quarantine-hours N   Skip crates.io versions newer than N hours.
                         Default: $VIBE_BUMP_QUARANTINE_HOURS, else 24.
  --skip-internal        Skip NEAT-AI-core pin refresh.
  --skip-external        Skip cargo update (crates.io).
  --skip-audit           Skip cargo audit.
  --require-audit        Treat a missing cargo-audit as a failure instead of
                         a loud skip. Default: $BUMP_DEPS_REQUIRE_AUDIT, else
                         off. Issue #619.
  --skip-build           Skip cargo build --release.
  --cargo-upgrade        Use 'cargo upgrade' (cargo-edit) to bump Cargo.toml
                         manifest versions instead of 'cargo update' (lockfile
                         only). Each candidate is still gated by the
                         quarantine window. Issue #101 — invoked per-PR by a
                         worker (the weekly schedule was removed by #105).
  --neat-core-sha SHA    Override upstream Develop SHA (testing).
  --manifest PATH        rust_scorer manifest scanned for the neat-core pin
                         (default: rust_scorer/Cargo.toml).
  --repo DIR             Repository root (default: cwd).
  --check-published TS H Internal helper: exit 0 if TS (ISO 8601) is older
                         than H hours, else exit 1. Used by tests.
  -h, --help             Show this message.

Exit codes:
  0  clean / no-op
  1  bump produced a non-passing tree (audit / build failure)
  2  usage error
EOF
}

QUARANTINE_HOURS="${VIBE_BUMP_QUARANTINE_HOURS:-24}"
SKIP_INTERNAL=0
SKIP_EXTERNAL=0
SKIP_AUDIT=0
REQUIRE_AUDIT="${BUMP_DEPS_REQUIRE_AUDIT:-0}"
SKIP_BUILD=0
CARGO_UPGRADE=0
NEAT_CORE_SHA_OVERRIDE=""
MANIFEST="rust_scorer/Cargo.toml"
REPO_DIR="."
CHECK_PUBLISHED_TS=""
CHECK_PUBLISHED_HOURS=""
MODE="run"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --quarantine-hours) QUARANTINE_HOURS="$2"; shift 2 ;;
    --skip-internal)    SKIP_INTERNAL=1; shift ;;
    --skip-external)    SKIP_EXTERNAL=1; shift ;;
    --skip-audit)       SKIP_AUDIT=1; shift ;;
    --require-audit)    REQUIRE_AUDIT=1; shift ;;
    --skip-build)       SKIP_BUILD=1; shift ;;
    --cargo-upgrade)    CARGO_UPGRADE=1; shift ;;
    --neat-core-sha)    NEAT_CORE_SHA_OVERRIDE="$2"; shift 2 ;;
    --manifest)         MANIFEST="$2"; shift 2 ;;
    --repo)             REPO_DIR="$2"; shift 2 ;;
    --check-published)
      MODE="check-published"
      CHECK_PUBLISHED_TS="${2:-}"
      CHECK_PUBLISHED_HOURS="${3:-}"
      if [[ -z "$CHECK_PUBLISHED_TS" || -z "$CHECK_PUBLISHED_HOURS" ]]; then
        echo "Usage error: --check-published requires <timestamp> <hours>" >&2
        exit 2
      fi
      shift 3
      ;;
    -h|--help)          usage; exit 0 ;;
    *)
      echo "Usage error: unknown option '$1'" >&2
      usage >&2
      exit 2
      ;;
  esac
done

# --- helpers ---------------------------------------------------------------

# Returns 0 if $1 (ISO 8601) is older than $2 hours, else 1. Exit 2 on parse
# error so callers can distinguish unparsable timestamps from "still fresh".
is_older_than_hours() {
  local published_at="$1" hours="$2"
  python3 - "$published_at" "$hours" <<'PY'
import sys, datetime
ts = sys.argv[1].strip()
try:
    hours = float(sys.argv[2])
except ValueError:
    sys.exit(2)
if ts.endswith("Z"):
    ts = ts[:-1] + "+00:00"
try:
    pub = datetime.datetime.fromisoformat(ts)
except ValueError:
    sys.exit(2)
if pub.tzinfo is None:
    pub = pub.replace(tzinfo=datetime.timezone.utc)
now = datetime.datetime.now(datetime.timezone.utc)
age_hours = (now - pub).total_seconds() / 3600.0
sys.exit(0 if age_hours >= hours else 1)
PY
}

if [[ "$MODE" == "check-published" ]]; then
  set +e
  is_older_than_hours "$CHECK_PUBLISHED_TS" "$CHECK_PUBLISHED_HOURS"
  rc=$?
  set -e
  if [[ "$rc" -eq 2 ]]; then
    echo "Error: invalid timestamp '$CHECK_PUBLISHED_TS'" >&2
    exit 2
  fi
  exit "$rc"
fi

if ! [[ "$QUARANTINE_HOURS" =~ ^[0-9]+$ ]]; then
  echo "Usage error: --quarantine-hours must be a non-negative integer (got '$QUARANTINE_HOURS')" >&2
  exit 2
fi

# Resolve the absolute manifest path so functions can be called regardless of
# the caller's working directory.
resolve_manifest_path() {
  local repo="$1" rel="$2"
  if [[ "$rel" = /* ]]; then
    printf '%s\n' "$rel"
  else
    printf '%s/%s\n' "${repo%/}" "$rel"
  fi
}

# Read the current `rev = "..."` pin for neat-core, supporting both the
# inline-table form (single line under [dependencies]) and the separate
# [dependencies.neat-core] section form. Empty output means "no SHA pin".
current_neat_core_rev() {
  local manifest="$1"
  awk '
    BEGIN { in_section = 0 }
    /^\[dependencies\.neat-core\]/ { in_section = 1; next }
    /^\[/ {
      if (in_section) in_section = 0
    }
    in_section && /^[[:space:]]*rev[[:space:]]*=[[:space:]]*"[^"]*"/ {
      match($0, /"[^"]*"/)
      print substr($0, RSTART + 1, RLENGTH - 2)
      exit
    }
    /^[[:space:]]*neat-core[[:space:]]*=.*rev[[:space:]]*=[[:space:]]*"[^"]*"/ {
      s = $0
      sub(/^.*rev[[:space:]]*=[[:space:]]*"/, "", s)
      sub(/".*$/, "", s)
      print s
      exit
    }
  ' "$manifest"
}

# Replace the neat-core rev pin in-place. Handles both inline-table and
# [dependencies.neat-core] section forms.
write_neat_core_rev() {
  local manifest="$1" new_sha="$2"
  awk -v new="$new_sha" '
    BEGIN { in_section = 0; done = 0 }
    {
      if (!done && $0 ~ /^\[dependencies\.neat-core\]/) {
        in_section = 1
        print
        next
      }
      if (in_section && $0 ~ /^\[/) {
        in_section = 0
      }
      if (!done && in_section && $0 ~ /^[[:space:]]*rev[[:space:]]*=[[:space:]]*"[^"]*"/) {
        sub(/"[^"]*"/, "\"" new "\"")
        done = 1
        print
        next
      }
      if (!done && $0 ~ /^[[:space:]]*neat-core[[:space:]]*=.*rev[[:space:]]*=[[:space:]]*"[^"]*"/) {
        sub(/rev[[:space:]]*=[[:space:]]*"[^"]*"/, "rev = \"" new "\"")
        done = 1
        print
        next
      }
      print
    }
  ' "$manifest" >"${manifest}.tmp"
  mv "${manifest}.tmp" "$manifest"
}

# Resolve upstream NEAT-AI-core Develop SHA. Honours --neat-core-sha for
# tests; falls back to `gh api`.
upstream_neat_core_sha() {
  if [[ -n "$NEAT_CORE_SHA_OVERRIDE" ]]; then
    printf '%s\n' "$NEAT_CORE_SHA_OVERRIDE"
    return 0
  fi
  if ! command -v gh >/dev/null 2>&1; then
    echo "Error: gh CLI not available; cannot resolve NEAT-AI-core Develop HEAD" >&2
    return 1
  fi
  gh api repos/stSoftwareAU/NEAT-AI-core/commits/Develop --jq .sha
}

# Look up the published-at timestamp for a specific crates.io version.
# Honours $BUMP_DEPS_PUBLISH_FIXTURE (a directory of <crate>-<version>.iso
# files) so tests can exercise the quarantine branches without network.
crate_published_at() {
  local crate="$1" version="$2"
  if [[ -n "${BUMP_DEPS_PUBLISH_FIXTURE:-}" ]]; then
    local f="${BUMP_DEPS_PUBLISH_FIXTURE}/${crate}-${version}.iso"
    if [[ -f "$f" ]]; then
      tr -d '\n' <"$f"
      printf '\n'
      return 0
    fi
    # Missing fixture → treat as ancient so the bump proceeds.
    printf '1970-01-01T00:00:00Z\n'
    return 0
  fi
  if ! command -v curl >/dev/null 2>&1; then
    echo "Error: curl required to query crates.io" >&2
    return 1
  fi
  local base="${BUMP_DEPS_CRATES_IO_URL:-https://crates.io/api/v1}"
  local payload
  if ! payload=$(curl -fsSL --user-agent "neat-ai-scorer-bump-deps" \
    "${base}/crates/${crate}/versions" 2>/dev/null); then
    echo "Error: crates.io request for ${crate} failed" >&2
    return 1
  fi
  printf '%s' "$payload" | python3 -c '
import json, sys
target = sys.argv[1]
data = json.load(sys.stdin)
for v in data.get("versions", []):
    if v.get("num") == target:
        print(v.get("created_at", ""))
        break
' "$version"
}

# --- stages ----------------------------------------------------------------

internal_changed=0
external_changed=0
internal_msg="skipped"
external_msg="skipped"
audit_msg="skipped"
build_msg="skipped"

bump_internal() {
  local manifest
  manifest="$(resolve_manifest_path "$REPO_DIR" "$MANIFEST")"
  if [[ ! -f "$manifest" ]]; then
    echo "Error: manifest '$manifest' not found" >&2
    internal_msg="error"
    return 1
  fi
  local current
  current="$(current_neat_core_rev "$manifest")"
  if [[ -z "$current" ]]; then
    internal_msg="path dependency (no SHA pin)"
    echo "internal: NEAT-AI-core resolved via path dependency — no SHA pin to refresh"
    return 0
  fi
  local upstream
  if ! upstream="$(upstream_neat_core_sha)"; then
    internal_msg="error"
    return 1
  fi
  upstream="${upstream//[$'\t\r\n ']}"
  if [[ -z "$upstream" ]]; then
    echo "Error: empty upstream NEAT-AI-core SHA" >&2
    internal_msg="error"
    return 1
  fi
  if [[ "$current" == "$upstream" ]]; then
    internal_msg="already pinned (${upstream:0:7})"
    echo "internal: NEAT-AI-core already pinned to ${upstream:0:7}"
    return 0
  fi
  write_neat_core_rev "$manifest" "$upstream"
  internal_changed=1
  internal_msg="${current:0:7} -> ${upstream:0:7}"
  echo "internal: NEAT-AI-core ${current:0:7} -> ${upstream:0:7}"
}

# Emit "<name>\t<version>" for every package recorded in a Cargo.lock.
lock_versions() {
  local lock="$1"
  [[ -f "$lock" ]] || return 0
  awk '
    /^\[\[package\]\]/ { name = ""; next }
    /^name = / { name = $3; gsub(/"/, "", name); next }
    /^version = / {
      if (name != "") {
        v = $3
        gsub(/"/, "", v)
        printf "%s\t%s\n", name, v
        name = ""
      }
    }
  ' "$lock"
}

# Apply a single vetted candidate. Captures cargo's own output in APPLY_ERR so
# a rejection is reported with its reason instead of being discarded (#619).
apply_one_candidate() {
  local crate="$1" version="$2" rc=0
  if [[ "$CARGO_UPGRADE" -eq 1 ]]; then
    APPLY_ERR="$( (cd "$REPO_DIR" && cargo upgrade -p "$crate@$version") 2>&1 )" || rc=$?
  else
    APPLY_ERR="$( (cd "$REPO_DIR" && cargo update -p "$crate" --precise "$version") 2>&1 )" || rc=$?
  fi
  return "$rc"
}

# Retry a set of crates in ONE `cargo update` so interlocked families move
# together (Issue #619). wasm-bindgen, js-sys and web-sys pin each other with
# `=` requirements, so every per-crate `--precise` apply is rejected while the
# rest of the graph stays locked — the family can only ever advance as a unit.
#
# A grouped resolve is more permissive than `--precise`, so the result is
# verified against the vetted candidate list: any package whose locked version
# changed to something the quarantine gate did not clear rolls the whole retry
# back. Sets REGROUP_ERR on failure.
regroup_apply() {
  local lock="${REPO_DIR%/}/Cargo.lock"
  REGROUP_ERR=""
  if [[ ! -f "$lock" ]]; then
    REGROUP_ERR="no Cargo.lock at ${lock} — cannot verify a regrouped update"
    return 1
  fi
  local backup
  backup="$(mktemp)"
  cp "$lock" "$backup"

  local args=() crate
  for crate in "$@"; do
    args+=(-p "$crate")
  done

  local rc=0 out
  out="$( (cd "$REPO_DIR" && cargo update "${args[@]}") 2>&1 )" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    REGROUP_ERR="$out"
    cp "$backup" "$lock"
    rm -f "$backup"
    return 1
  fi

  local changed violation="" entry
  changed="$(comm -13 <(lock_versions "$backup" | sort) <(lock_versions "$lock" | sort))"
  while IFS= read -r entry; do
    [[ -z "$entry" ]] && continue
    if ! printf '%s\n' "$VETTED_CANDIDATES" | grep -Fqx -- "$entry"; then
      violation+="${entry//$'\t'/ } "
    fi
  done <<<"$changed"

  if [[ -n "$violation" ]]; then
    cp "$backup" "$lock"
    rm -f "$backup"
    REGROUP_ERR="regrouped update pulled in unvetted versions (not cleared by the ${QUARANTINE_HOURS}h quarantine): ${violation% }"
    return 1
  fi

  rm -f "$backup"
  return 0
}

bump_external() {
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not available" >&2
    external_msg="error"
    return 1
  fi
  # Issue #101: --cargo-upgrade switches the driver to `cargo upgrade`
  # (cargo-edit) so a worker preparing a PR can bump Cargo.toml manifest
  # versions through the same quarantine gate. The weekly schedule that
  # originally needed this flag was removed by Issue #105.
  local dry_cmd apply_label
  if [[ "$CARGO_UPGRADE" -eq 1 ]]; then
    if ! cargo upgrade --version >/dev/null 2>&1; then
      echo "Error: cargo upgrade not available — install with 'cargo install cargo-edit'" >&2
      external_msg="error"
      return 1
    fi
    dry_cmd=(cargo upgrade --dry-run)
    apply_label="upgrade"
  else
    dry_cmd=(cargo update --dry-run)
    apply_label="update"
  fi
  local dry_log dry_rc=0
  dry_log="$( (cd "$REPO_DIR" && "${dry_cmd[@]}") 2>&1 )" || dry_rc=$?
  # A dry run that cannot resolve the graph is a real failure, not "no
  # updates" — reporting it as a clean no-op would hide a broken tree (#619).
  if [[ "$dry_rc" -ne 0 ]]; then
    printf '%s\n' "$dry_log" >&2
    echo "Error: 'cargo ${apply_label} --dry-run' failed (exit ${dry_rc}) — dependency graph could not be resolved" >&2
    external_msg="error"
    return 1
  fi

  local applied=0 deferred=0 failed=0
  local cand_crates=() cand_versions=()
  VETTED_CANDIDATES=""
  local line
  while IFS= read -r line; do
    # Match lines like:
    #   Updating  clap v4.5.20 -> v4.5.21       (cargo update)
    #   Bumping   serde v1.0.210 -> v1.0.211    (cargo update output variant)
    #   Upgrading clap v4.5.20 -> v4.5.21       (cargo upgrade)
    if [[ "$line" =~ (Updating|Bumping|Upgrading)[[:space:]]+([a-zA-Z0-9_-]+)[[:space:]]+v([0-9A-Za-z.+-]+)[[:space:]]+-\>[[:space:]]+v([0-9A-Za-z.+-]+) ]]; then
      local crate="${BASH_REMATCH[2]}"
      local new_v="${BASH_REMATCH[4]}"
      # Internal stSoftware path deps don't appear in cargo update output;
      # this filter just guards against future git+rev pins.
      if [[ "$crate" == "neat-core" ]]; then
        continue
      fi
      local published_at
      if ! published_at="$(crate_published_at "$crate" "$new_v")"; then
        echo "  skip: $crate $new_v (publish time lookup failed)"
        failed=$((failed + 1))
        continue
      fi
      if is_older_than_hours "$published_at" "$QUARANTINE_HOURS"; then
        cand_crates+=("$crate")
        cand_versions+=("$new_v")
        VETTED_CANDIDATES+="${crate}"$'\t'"${new_v}"$'\n'
      else
        deferred=$((deferred + 1))
        echo "  defer: $crate $new_v (within ${QUARANTINE_HOURS}h quarantine, published $published_at)"
      fi
    fi
  done <<<"$dry_log"

  local retry_crates=() retry_versions=() retry_errors=()
  local i
  for i in "${!cand_crates[@]}"; do
    if apply_one_candidate "${cand_crates[$i]}" "${cand_versions[$i]}"; then
      applied=$((applied + 1))
      echo "  bump: ${cand_crates[$i]} -> ${cand_versions[$i]}"
    else
      retry_crates+=("${cand_crates[$i]}")
      retry_versions+=("${cand_versions[$i]}")
      retry_errors+=("${APPLY_ERR}")
    fi
  done

  # Interlocked crates rejected one at a time get one grouped retry.
  if [[ "${#retry_crates[@]}" -gt 0 ]]; then
    if [[ "$CARGO_UPGRADE" -eq 1 ]]; then
      # `cargo upgrade` rewrites Cargo.toml, so the Cargo.lock verification
      # below does not apply — report the rejection with cargo's own reason.
      REGROUP_ERR="cargo upgrade does not support the grouped lockfile retry"
    else
      echo "  regroup: retrying ${#retry_crates[@]} interlocked crate(s) in one cargo update"
      if regroup_apply "${retry_crates[@]}"; then
        for i in "${!retry_crates[@]}"; do
          applied=$((applied + 1))
          echo "  bump: ${retry_crates[$i]} -> ${retry_versions[$i]} (regrouped)"
        done
        retry_crates=()
        retry_errors=()
      fi
    fi
  fi

  if [[ "${#retry_crates[@]}" -gt 0 ]]; then
    # Fail loud: report cargo's own reason for each rejection, then the reason
    # the grouped retry could not rescue them either.
    for i in "${!retry_crates[@]}"; do
      failed=$((failed + 1))
      echo "  fail: ${retry_crates[$i]} -> ${retry_versions[$i]} (cargo ${apply_label} rejected)"
      printf '%s\n' "${retry_errors[$i]}" >&2
    done
    printf 'regroup: %s\n' "$REGROUP_ERR" >&2
  fi

  if [[ "$applied" -gt 0 ]]; then
    external_changed=1
  fi
  if [[ "$applied" -eq 0 && "$deferred" -eq 0 && "$failed" -eq 0 ]]; then
    external_msg="no updates"
  else
    external_msg="${applied} bumped, ${deferred} deferred, ${failed} failed"
  fi
  echo "external: $external_msg"
}

run_audit() {
  if ! cargo audit --version >/dev/null 2>&1; then
    # Issue #619: a cargo-audit missing from the unattended PATH is a tooling
    # gap, not a bump rejection. Failing here reverted every bump and left the
    # repo with no dependency updates at all — a worse security outcome than
    # deferring the scan to `.github/workflows/cargo-audit.yml`, which runs
    # `cargo audit` on every PR. The skip is loud, never silent.
    if [[ "$REQUIRE_AUDIT" -eq 1 ]]; then
      echo "Error: cargo audit not available — install with 'cargo install cargo-audit --locked'" >&2
      audit_msg="error"
      return 1
    fi
    echo "Warning: cargo audit not available — advisory scan deferred to CI (.github/workflows/cargo-audit.yml runs it on every PR)." >&2
    echo "Warning: install locally with 'cargo install cargo-audit --locked'; pass --require-audit to make this fatal." >&2
    audit_msg="skipped (cargo-audit not installed)"
    echo "audit: SKIPPED (cargo-audit not installed)"
    return 0
  fi
  local audit_log
  if audit_log="$(cd "$REPO_DIR" && cargo audit 2>&1)"; then
    audit_msg="ok"
    echo "audit: ok"
    return 0
  fi
  # Surface the first advisory ID + offending crate so the worker log shows
  # exactly why the bump was rejected.
  local first
  first=$(printf '%s\n' "$audit_log" | awk '
    /^[[:space:]]*ID:[[:space:]]/    { id = $2 }
    /^[[:space:]]*Crate:[[:space:]]/ {
      if (id != "") { print "audit: FAILED — " $2 " (" id ")"; exit }
    }
  ')
  if [[ -z "$first" ]]; then
    first="audit: FAILED (see cargo audit output above)"
  fi
  audit_msg="failed"
  printf '%s\n' "$audit_log" >&2
  printf '%s\n' "$first" >&2
  return 1
}

run_build() {
  if (cd "$REPO_DIR" && cargo build --release --workspace) >&2; then
    build_msg="ok"
    echo "build: ok"
    return 0
  fi
  build_msg="failed"
  echo "build: FAILED" >&2
  return 1
}

# --- main ------------------------------------------------------------------

if [[ "$SKIP_INTERNAL" -eq 0 ]]; then bump_internal; fi
if [[ "$SKIP_EXTERNAL" -eq 0 ]]; then bump_external; fi
if [[ "$SKIP_AUDIT"    -eq 0 ]]; then run_audit;     fi
if [[ "$SKIP_BUILD"    -eq 0 ]]; then run_build;     fi

if [[ "$internal_changed" -eq 0 && "$external_changed" -eq 0 ]]; then
  echo "bump-deps: no bumps (internal=${internal_msg}; external=${external_msg}; audit=${audit_msg}; build=${build_msg})"
else
  echo "bump-deps: internal=${internal_msg}; external=${external_msg}; audit=${audit_msg}; build=${build_msg}"
fi
