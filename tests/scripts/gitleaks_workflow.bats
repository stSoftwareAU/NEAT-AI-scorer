#!/usr/bin/env bats
# Tests for scripts/check-gitleaks-workflow.sh — Issue #21.
#
# Exercises the hardening validator with synthetic workflow YAML in
# temporary directories so behaviour (exit codes, reported failures) is
# verified end-to-end without touching the real workflow file.

setup() {
  SCRIPT_UNDER_TEST="${BATS_TEST_DIRNAME}/../../scripts/check-gitleaks-workflow.sh"
  [ -x "$SCRIPT_UNDER_TEST" ] || chmod +x "$SCRIPT_UNDER_TEST"

  TMP_WF="$(mktemp -d)"
  export TMP_WF
}

teardown() {
  rm -rf "$TMP_WF"
}

# Canonical hardened workflow. Individual failure tests mutate this fixture
# to drop or break one rule at a time.
write_hardened_workflow() {
  local file="$1"
  cat >"$file" <<'EOF'
name: Gitleaks

on:
  pull_request:
    branches: ["*"]

permissions:
  contents: read

jobs:
  gitleaks:
    name: Gitleaks Secrets Detection
    runs-on: ubuntu-latest
    # Version is pinned so PR scans are reproducible and a compromised
    # release asset cannot silently replace the scanner.
    env:
      GITLEAKS_VERSION: "8.24.2"
      GITLEAKS_SHA256: "0000000000000000000000000000000000000000000000000000000000000000"
    steps:
      - name: Checkout code
        uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Install pinned Gitleaks binary
        run: |
          set -euo pipefail
          archive="gitleaks_${GITLEAKS_VERSION}_linux_x64.tar.gz"
          curl -sSfL "https://github.com/gitleaks/gitleaks/releases/download/v${GITLEAKS_VERSION}/${archive}" -o "${archive}"
          echo "${GITLEAKS_SHA256}  ${archive}" | sha256sum --check -
          tar -xzf "${archive}" gitleaks
          sudo install -m 0755 gitleaks /usr/local/bin/gitleaks

      - name: Fetch base ref for diff scan
        run: |
          set -euo pipefail
          git fetch --no-tags --depth=0 origin "${{ github.base_ref }}"

      - name: Run Gitleaks on PR diff range
        run: |
          set -euo pipefail
          gitleaks detect \
            --source . \
            --log-opts="origin/${{ github.base_ref }}..HEAD" \
            --redact \
            --verbose
EOF
}

@test "passes on a hardened workflow" {
  write_hardened_workflow "$TMP_WF/gitleaks.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/gitleaks.yml"
  [ "$status" -eq 0 ]
  [[ "$output" == *"pinned Gitleaks release URL present"* ]]
  [[ "$output" == *"archive checksum verification present"* ]]
  [[ "$output" == *"--log-opts limits scan to PR diff range"* ]]
  [[ "$output" == *"strict bash (set -euo pipefail) present"* ]]
}

@test "fails when the workflow uses gitleaks-action instead of a pinned binary" {
  cat >"$TMP_WF/gitleaks.yml" <<'EOF'
name: Gitleaks
on: [pull_request]
jobs:
  gitleaks:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: gitleaks/gitleaks-action@v2
EOF
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/gitleaks.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"uses gitleaks-action"* ]]
}

@test "fails when the pinned release URL is missing" {
  write_hardened_workflow "$TMP_WF/gitleaks.yml"
  # Replace the pinned URL with an unpinned command.
  sed -i.bak 's|https://github.com/gitleaks/gitleaks/releases/download/v${GITLEAKS_VERSION}/${archive}|https://example.com/gitleaks-latest.tar.gz|' "$TMP_WF/gitleaks.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/gitleaks.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no pinned Gitleaks release URL"* ]]
}

@test "fails when there is no comment documenting the pin rationale" {
  write_hardened_workflow "$TMP_WF/gitleaks.yml"
  # Strip every comment line from the fixture.
  grep -v '^[[:space:]]*#' "$TMP_WF/gitleaks.yml" >"$TMP_WF/gitleaks.stripped.yml"
  mv "$TMP_WF/gitleaks.stripped.yml" "$TMP_WF/gitleaks.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/gitleaks.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no comment explaining why the version is pinned"* ]]
}

@test "fails when the archive checksum is not verified" {
  write_hardened_workflow "$TMP_WF/gitleaks.yml"
  # Remove the sha256sum verification line.
  grep -v 'sha256sum' "$TMP_WF/gitleaks.yml" >"$TMP_WF/gitleaks.stripped.yml"
  mv "$TMP_WF/gitleaks.stripped.yml" "$TMP_WF/gitleaks.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/gitleaks.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no SHA256 checksum verification"* ]]
}

@test "fails when the scan does not limit to base..HEAD" {
  write_hardened_workflow "$TMP_WF/gitleaks.yml"
  sed -i.bak 's|--log-opts="origin/${{ github.base_ref }}..HEAD"|--no-git|' "$TMP_WF/gitleaks.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/gitleaks.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no --log-opts=<base>..HEAD"* ]]
}

@test "fails when the base ref is not explicitly fetched" {
  write_hardened_workflow "$TMP_WF/gitleaks.yml"
  # Remove the git fetch step line.
  grep -v 'git fetch' "$TMP_WF/gitleaks.yml" >"$TMP_WF/gitleaks.stripped.yml"
  mv "$TMP_WF/gitleaks.stripped.yml" "$TMP_WF/gitleaks.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/gitleaks.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no explicit 'git fetch origin <base_ref>'"* ]]
}

@test "fails when strict bash is missing from run blocks" {
  write_hardened_workflow "$TMP_WF/gitleaks.yml"
  grep -v 'set -euo pipefail' "$TMP_WF/gitleaks.yml" >"$TMP_WF/gitleaks.stripped.yml"
  mv "$TMP_WF/gitleaks.stripped.yml" "$TMP_WF/gitleaks.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/gitleaks.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no 'set -euo pipefail'"* ]]
}

@test "fails when the workflow does not invoke gitleaks detect" {
  write_hardened_workflow "$TMP_WF/gitleaks.yml"
  sed -i.bak 's|gitleaks detect|gitleaks version|' "$TMP_WF/gitleaks.yml"
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/gitleaks.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"no 'gitleaks detect' invocation"* ]]
}

@test "reports an error when the workflow file does not exist" {
  run "$SCRIPT_UNDER_TEST" --workflow "$TMP_WF/does-not-exist.yml"
  [ "$status" -ne 0 ]
  [[ "$output" == *"not found"* ]]
}

@test "unknown flag prints usage and exits non-zero" {
  run "$SCRIPT_UNDER_TEST" --nonsense
  [ "$status" -ne 0 ]
  [[ "$output" == *"Usage"* ]]
}

@test "real repository gitleaks workflow satisfies every hardening rule" {
  run "$SCRIPT_UNDER_TEST"
  [ "$status" -eq 0 ]
  [[ "$output" != *"FAIL"* ]]
}
