#!/usr/bin/env bats
# Tests for Rust API Guidelines (C-METADATA) — Issue #117.
#
# The `rust_scorer` crate must declare `repository` so downstream tooling
# (cargo publish, SBOMs, cargo doc, IDE tooltips) can resolve the source
# back to GitHub. The workspace pins `neat-core` via a `path:` dep on a
# sibling clone, so the upstream repository URL is the only canonical
# pointer to where the code came from.

setup() {
  REPO_ROOT="${BATS_TEST_DIRNAME}/../.."
  CARGO_TOML="${REPO_ROOT}/rust_scorer/Cargo.toml"
}

@test "rust_scorer Cargo.toml declares a repository URL" {
  run grep -E '^repository[[:space:]]*=' "$CARGO_TOML"
  [ "$status" -eq 0 ]
  [[ "$output" == *"https://github.com/stSoftwareAU/NEAT-AI-scorer"* ]]
}

@test "rust_scorer Cargo.toml declares a readme path" {
  run grep -E '^readme[[:space:]]*=' "$CARGO_TOML"
  [ "$status" -eq 0 ]
}

@test "rust_scorer Cargo.toml readme points to an existing file" {
  readme_line="$(grep -E '^readme[[:space:]]*=' "$CARGO_TOML")"
  # Extract the quoted value.
  readme_path="$(printf '%s' "$readme_line" | sed -E 's/^readme[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/')"
  [ -n "$readme_path" ]
  [ -f "${REPO_ROOT}/rust_scorer/${readme_path}" ]
}

@test "cargo metadata exposes the repository field for rust_scorer" {
  if ! command -v cargo >/dev/null 2>&1; then
    skip "cargo not available in this environment"
  fi
  pushd "$REPO_ROOT" >/dev/null
  run cargo metadata --no-deps --format-version 1
  popd >/dev/null
  [ "$status" -eq 0 ]
  # The repository URL must appear in the rust_scorer package metadata.
  [[ "$output" == *"\"repository\":\"https://github.com/stSoftwareAU/NEAT-AI-scorer\""* ]]
}
