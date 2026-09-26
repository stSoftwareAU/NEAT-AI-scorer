#!/usr/bin/env bash
# Validate the Dependabot security-update channel (Issue #658).
#
# `bump-deps.sh` only runs per-PR through the automation worker (#105 removed
# its weekly schedule) and `cargo-audit.yml` only *detects* advisories, so
# without `.github/dependabot.yml` nothing opens a bump PR when a fix ships.
# The config gives the repo a trigger that does not depend on unrelated work.
#
# The config must:
#   1. Declare `version: 2`.
#   2. Carry an `updates` entry for `package-ecosystem: cargo`.
#   3. Point that entry at the workspace root (`directory: "/"`, or a
#      `directories` list containing it) — where `Cargo.lock` lives.
#   4. Schedule it `daily` or `weekly`, so a bump never waits a month.
#   5. Set a `cooldown` of at least one day (`default-days >= 1`), mirroring
#      `bump-deps.sh`'s 24-hour quarantine. Dependabot security updates are
#      exempt from cooldown, so advisory fixes still land immediately.
#   6. Ignore `neat-core` — `scripts/family-pins.sh` owns that release-tag pin
#      (Issue #630), and a second bumper would race it.
#
# Takes an optional `--config PATH` so BATS tests can exercise fixtures.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/check-harness.sh
source "$SCRIPT_DIR/lib/check-harness.sh"

usage() {
  cat <<'EOF'
Usage: check-dependabot-config.sh [--config PATH]

Options:
  --config PATH  Dependabot config to validate
                 (default: .github/dependabot.yml relative to the repo root).
  -h, --help     Show this message.

Exits 0 when the config satisfies every rule in the script header.
Exits non-zero with a descriptive message otherwise.
EOF
}

parse_check_args --config ".github/dependabot.yml" "$@"
CONFIG="$CHECK_TARGET"
check_require_file "$CONFIG" "Dependabot config"
check_subject "$CONFIG"

# The rules are evaluated by a small indentation-aware reader (no PyYAML
# dependency) that prints one `OK<TAB>msg` / `FAIL<TAB>msg` line per rule.
results="$(
  python3 - "$CONFIG" <<'PY'
import re
import sys

with open(sys.argv[1], "r", encoding="utf-8") as fh:
    raw_lines = fh.read().splitlines()


def unquote(value):
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        return value[1:-1]
    return value


def strip_comment(line):
    # Drop a trailing `# comment` that sits outside quotes.
    quote = None
    for i, ch in enumerate(line):
        if quote:
            if ch == quote:
                quote = None
        elif ch in "\"'":
            quote = ch
        elif ch == "#" and (i == 0 or line[i - 1] in " \t"):
            return line[:i].rstrip()
    return line.rstrip()


lines = []
for raw in raw_lines:
    line = strip_comment(raw)
    if line.strip():
        lines.append(line)


def indent_of(line):
    return len(line) - len(line.lstrip(" "))


def parse_block(block):
    """Flatten a list item's lines into (path, value) pairs.

    Nested keys join with '.', and a `- ` list element appends '[]' to its
    parent key, e.g. `ignore[].dependency-name`.
    """
    pairs = []
    stack = []  # (indent, path)
    for line in block:
        ind = indent_of(line)
        text = line.strip()
        is_item = text.startswith("- ") or text == "-"
        while stack and stack[-1][0] >= ind:
            stack.pop()
        parent = stack[-1][1] if stack else ""
        if is_item:
            parent = parent + "[]"
            text = text[1:].strip()
            ind += 2
            if not text:
                continue
        match = re.match(r"^([A-Za-z0-9_-]+):\s*(.*)$", text)
        if not match:
            pairs.append((parent, unquote(text)))
            continue
        key, value = match.group(1), match.group(2)
        path = f"{parent}.{key}" if parent else key
        if value:
            pairs.append((path, unquote(value)))
        else:
            stack.append((ind, path))
    return pairs


version = None
entries = []
current = None
updates_indent = None
item_indent = None
for line in lines:
    ind = indent_of(line)
    text = line.strip()
    if ind == 0:
        updates_indent = None
        if current is not None:
            entries.append(current)
            current = None
        match = re.match(r"^version:\s*(.*)$", text)
        if match:
            version = unquote(match.group(1))
        if text == "updates:":
            updates_indent = 0
        continue
    if updates_indent is None:
        continue
    if text.startswith("- ") and (item_indent is None or ind == item_indent):
        item_indent = ind
        if current is not None:
            entries.append(current)
        # Re-indent the item's first line so its keys align with the rest.
        current = [" " * (ind + 2) + text[2:]]
    elif current is not None:
        current.append(line)
if current is not None:
    entries.append(current)

parsed = [parse_block(entry) for entry in entries]


def values(pairs, path):
    return [v for p, v in pairs if p == path]


def report(passed, ok_msg, fail_msg):
    print(("OK\t" + ok_msg) if passed else ("FAIL\t" + fail_msg))


report(version == "2", "declares version: 2",
       f"must declare version: 2 (found {version!r})")

cargo = [p for p in parsed if "cargo" in values(p, "package-ecosystem")]
if not cargo:
    print("FAIL\tno updates entry for package-ecosystem: cargo — add one so "
          "Dependabot opens crate bump PRs")
    sys.exit(0)
report(True, "has an updates entry for package-ecosystem: cargo", "")


def covers_root(pairs):
    dirs = values(pairs, "directory") + values(pairs, "directories[]")
    return any(d.rstrip("/") == "" for d in dirs)


rooted = [p for p in cargo if covers_root(p)]
report(bool(rooted), "cargo entry covers the workspace root directory",
       "cargo entry directory must be \"/\" (the workspace root holding "
       "Cargo.lock)")
entry = rooted[0] if rooted else cargo[0]

interval = values(entry, "schedule.interval")
report(bool(interval) and interval[0] in ("daily", "weekly"),
       f"cargo schedule interval is {interval[0] if interval else ''}",
       "cargo schedule interval must be daily or weekly (found "
       f"{interval[0] if interval else 'none'})")

cooldown = values(entry, "cooldown.default-days")
cooldown_ok = bool(cooldown) and cooldown[0].isdigit() and int(cooldown[0]) >= 1
report(cooldown_ok,
       f"cargo cooldown default-days is {cooldown[0] if cooldown else ''}",
       "cargo cooldown default-days must be >= 1 to mirror bump-deps.sh's "
       "24-hour quarantine")

report("neat-core" in values(entry, "ignore[].dependency-name"),
       "cargo entry ignores neat-core (owned by scripts/family-pins.sh)",
       "cargo entry must ignore neat-core — scripts/family-pins.sh owns that "
       "pin (Issue #630)")
PY
)"

while IFS=$'\t' read -r verdict message; do
  case "$verdict" in
    OK) ok "$message" ;;
    FAIL) fail "$message" ;;
    *) fail "unexpected validator output: $verdict $message" ;;
  esac
done <<<"$results"

exit "$EXIT_CODE"
