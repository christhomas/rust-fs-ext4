#!/usr/bin/env bash
# Find rust-fs-core's output-budget.sh, the wrapper every test tier runs under.
#
# WHY THIS FILE EXISTS. The wrapper belongs to rust-fs-core, because core is
# the one crate every filesystem and image driver already depends on. For a
# while each driver kept its own copy instead: measured on 2026-09-22 the
# family had three divergent copies reached four different ways, each
# repository internally consistent and nothing comparing them. This script is
# how a driver uses core's copy without keeping one of its own.
#
# TWO PLACES, BECAUSE THERE ARE TWO KINDS OF CHECKOUT -- and that is a
# permanent fact about who is running the code, not a stage on the way
# somewhere:
#
#   1. ../rust-fs-core, the sibling. Preferred. Somebody developing against
#      this family has core checked out beside this repository, and may be
#      part-way through changing the wrapper itself. Their change is the one
#      that should run; a published copy of what it used to be is not.
#
#   2. The packaged Cargo dependency. For a checkout with no siblings beside
#      it -- somebody using the crate rather than developing it. `cargo
#      metadata` locates the resolved package without this script needing to
#      know anything about CARGO_HOME's layout.
#
# ONE RULE FOR BOTH: the script must answer `--version` with the API this
# repository was written against. The contract is what matters and is what a
# breaking revision would change first.
#
# THERE IS DELIBERATELY NO CHECKSUM. An earlier version pinned the wrapper's
# SHA-256, which was wrong twice over. It refused the sibling edits that are
# the entire reason the sibling is preferred -- one added comment line in core
# stopped the test tiers of five repositories. And it bought nothing for the
# packaged copy, whose bytes are already fixed by the version pinned in
# Cargo.toml and immutable once published. What it did buy was a hash to keep
# in step across five repositories on every core release, and a hard failure
# for whoever forgot one.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# FS_CORE_ROOT overrides the sibling location for an unusual layout. It is not
# a way to point at a different script: whatever it names is still checked.
CORE_ROOT="${FS_CORE_ROOT:-$REPO/../rust-fs-core}"
CORE_PACKAGE="am-fs-core"
SCRIPT_REL="scripts/output-budget.sh"
EXPECTED_API="rust-fs-core-output-budget 1"

die() {
    echo "resolve-output-budget.sh: $*" >&2
    exit 1
}

speaks_the_api() {
    local path="$1" version
    [ -f "$path" ] || return 1
    version="$(bash "$path" --version 2>/dev/null || true)"
    [ "$version" = "$EXPECTED_API" ] || return 1
    printf '%s\n' "$path"
}

# A PRESENT-BUT-WRONG SIBLING IS FATAL, not a reason to try Cargo. If the
# sibling is there it is what the developer is working on, and quietly grading
# their run with a published copy instead would hide the change they are
# testing.
if [ -f "$CORE_ROOT/$SCRIPT_REL" ]; then
    speaks_the_api "$CORE_ROOT/$SCRIPT_REL" && exit 0
    die "the sibling does not answer '$EXPECTED_API': $CORE_ROOT/$SCRIPT_REL
  Changing core's wrapper locally is fine and is why the sibling is preferred.
  Changing its CONTRACT means changing the API version with it, and updating
  every consumer to expect the new one. 'chore siblings' restores the pin."
fi

metadata="$(mktemp)"
metadata_err="$(mktemp)"
trap 'rm -f "$metadata" "$metadata_err"' EXIT
# --manifest-path, not the working directory. `cargo metadata` otherwise
# searches upward from wherever the caller happened to be standing, so the
# same script resolved a different workspace -- or none -- depending on cwd,
# while the sibling branch above is derived from BASH_SOURCE and does not.
# One of the two being cwd-dependent is a trap; now neither is.
if cargo metadata --format-version 1 --locked --manifest-path "$REPO/Cargo.toml" \
        >"$metadata" 2>"$metadata_err"; then
    package_root="$(python3 - "$metadata" "$CORE_PACKAGE" <<'PY'
import json
import pathlib
import sys

metadata_path, package_name = sys.argv[1:]
data = json.loads(pathlib.Path(metadata_path).read_text())
for package in data.get("packages", []):
    if package.get("name") == package_name:
        print(pathlib.Path(package["manifest_path"]).parent)
        break
PY
)"
    if [ -n "$package_root" ]; then
        package_script="$package_root/$SCRIPT_REL"
        speaks_the_api "$package_script" && exit 0
        die "the resolved $CORE_PACKAGE package has no $SCRIPT_REL answering '$EXPECTED_API': $package_root"
    fi
fi

echo "resolve-output-budget.sh: neither a sibling nor a resolved Cargo package supplied the wrapper" >&2
if [ -s "$metadata_err" ]; then
    sed 's/^/  cargo: /' "$metadata_err" >&2
fi
echo "  expected API: $EXPECTED_API" >&2
echo "  sibling path: $CORE_ROOT/$SCRIPT_REL" >&2
echo "  run 'chore siblings', or depend on a $CORE_PACKAGE release containing $SCRIPT_REL." >&2
exit 1
