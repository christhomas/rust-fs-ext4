#!/usr/bin/env bash
# Resolve the canonical rust-fs-core output-budget wrapper.
#
# WHY THIS FILE EXISTS. Every filesystem and image driver in this family runs
# its test tiers under the same output budget, and for a while every one of
# them kept its own copy of the script that enforces it. Measured on
# 2026-09-22 there were THREE divergent copies reached FOUR different ways:
# rust-fs-core's canonical one, a staler copy inside fs-linux-test-harness
# that ext4/xfs/btrfs pointed at, and a private fork in rust-img-qcow2. Each
# repository was internally consistent, and nothing compared them -- the same
# failure `chore siblings` exists to make unrepresentable, one layer up.
#
# The wrapper belongs to rust-fs-core because core is the one crate every
# driver already depends on. This script is how a driver finds core's copy
# without keeping one of its own.
#
# IT REFUSES RATHER THAN FALLS BACK. A wrapper that silently differs from the
# canonical one is worse than a missing one: the budget still passes, the
# numbers still look measured, and two repositories quietly grade their output
# by different rules. So the script is checked two ways before it is used --
# a SHA-256 of the exact bytes, and the `--version` string that names its API
# -- and anything else is an error with the reason on stderr.
#
# TWO PLACES ARE SEARCHED, IN THIS ORDER, AND THE ORDER MATTERS:
#
#   1. The sibling checkout at ../rust-fs-core. Preferred, so that coordinated
#      local changes to core are actually exercised by its consumers instead
#      of being masked by a published copy.
#   2. The packaged Cargo dependency. This is what makes a standalone checkout
#      work -- CI, or a clone with no siblings beside it -- because `cargo
#      metadata` can locate a registry package without this script needing to
#      know anything about CARGO_HOME's layout.
#
# EXPECTED_SHA256 BELOW GOVERNS THE PACKAGED COPY ONLY. A sibling is not
# checksummed, so day-to-day work on core needs no change here. When core
# publishes a new release whose wrapper differs, the digest moves with the
# version pin in Cargo.toml -- both describe the same published artifact.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# FS_CORE_ROOT overrides the sibling location for an unusual layout. It is not
# a way to point at a different script: whatever it names is still checksummed.
CORE_ROOT="${FS_CORE_ROOT:-$REPO/../rust-fs-core}"
CORE_PACKAGE="am-fs-core"
SCRIPT_REL="scripts/output-budget.sh"
EXPECTED_API="rust-fs-core-output-budget 1"
# The digest of the canonical script supplied by rust-fs-core.
EXPECTED_SHA256="5f7fee1f985c1285b04640ae247fcb4ef6e8cc3d63369675610605a2e29c0b99"

die() {
    echo "resolve-output-budget.sh: $*" >&2
    exit 1
}

sha256() {
    # Linux ships sha256sum, macOS ships shasum. Both are in the base system,
    # so requiring one of the two adds no dependency to either platform.
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        die "sha256sum or shasum is required to verify $1"
    fi
}

# THE TWO SOURCES ARE CHECKED DIFFERENTLY, AND THAT IS THE POINT.
#
# A SIBLING IS A WORKING COPY. The whole reason it is preferred is that a
# developer may be part-way through changing core's script and wants the
# consumers to run THAT, not a published copy of what it used to be. Checking
# its bytes against a pinned digest would refuse every such change -- adding
# one comment line to core would stop the test tiers of five repositories --
# so the sibling is held to its CONTRACT and not to its content: it must
# answer --version with the API this repository was written against.
#
# A CARGO PACKAGE IS AN IMMUTABLE ARTIFACT. Nobody is mid-edit inside
# ~/.cargo/registry, the bytes are fixed by the version already pinned in
# Cargo.toml, and a digest there costs nothing and catches a corrupted or
# tampered cache. So that one is held to both.
validate_api() {
    local path="$1" version
    [ -f "$path" ] || return 1
    version="$(bash "$path" --version 2>/dev/null || true)"
    [ "$version" = "$EXPECTED_API" ] || return 1
    printf '%s\n' "$path"
}

validate_api_and_digest() {
    local path="$1" actual
    [ -f "$path" ] || return 1
    actual="$(sha256 "$path")"
    [ "$actual" = "$EXPECTED_SHA256" ] || return 1
    validate_api "$path"
}

# A PRESENT-BUT-WRONG SIBLING IS FATAL, not a reason to try Cargo. If the
# sibling exists it is what the developer is working on, and quietly grading
# their run with a published copy instead would hide exactly the change they
# are trying to test.
if [ -f "$CORE_ROOT/$SCRIPT_REL" ]; then
    validate_api "$CORE_ROOT/$SCRIPT_REL" && exit 0
    die "the sibling script does not answer '$EXPECTED_API': $CORE_ROOT/$SCRIPT_REL
  A local change to core's wrapper is fine and is why the sibling is preferred,
  but one that changes the contract must change the API version with it, and
  every consumer must be updated to expect the new one. Run 'chore siblings' to
  return the sibling to its pinned tag."
fi

metadata="$(mktemp)"
metadata_err="$(mktemp)"
trap 'rm -f "$metadata" "$metadata_err"' EXIT
if cargo metadata --format-version 1 --locked >"$metadata" 2>"$metadata_err"; then
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
        validate_api_and_digest "$package_script" && exit 0
        die "resolved Cargo package has no verified $SCRIPT_REL: $package_root"
    fi
fi

echo "resolve-output-budget.sh: no sibling or resolved Cargo package supplied the canonical script" >&2
if [ -s "$metadata_err" ]; then
    sed 's/^/  cargo: /' "$metadata_err" >&2
fi
echo "  expected API: $EXPECTED_API" >&2
echo "  expected SHA-256: $EXPECTED_SHA256" >&2
echo "  sibling path: $CORE_ROOT/$SCRIPT_REL" >&2
echo "  install a rust-fs-core release containing $SCRIPT_REL, or run 'chore siblings'." >&2
exit 1
