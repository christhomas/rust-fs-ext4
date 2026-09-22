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
# WHEN THE CANONICAL SCRIPT CHANGES, EXPECTED_SHA256 BELOW MUST CHANGE WITH
# IT, in every repository carrying this file, and core must publish a release
# containing the new bytes before a standalone checkout can find them. A comment
# edit in core is enough to trip this. That is deliberate: it is the cost of
# having exactly one copy, and it is cheaper than discovering the divergence
# from a budget that passed when it should not have.
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

# Both checks, not either. The digest proves the bytes are the ones this
# repository was written against; the API string proves the script is the
# thing we think it is rather than an unrelated file with a lucky hash, and it
# is what a future incompatible revision would change first.
validate() {
    local path="$1" actual version
    [ -f "$path" ] || return 1

    actual="$(sha256 "$path")"
    [ "$actual" = "$EXPECTED_SHA256" ] || return 1

    version="$(bash "$path" --version 2>/dev/null || true)"
    [ "$version" = "$EXPECTED_API" ] || return 1
    printf '%s\n' "$path"
}

# A PRESENT-BUT-WRONG SIBLING IS FATAL, not a reason to try Cargo. If the
# sibling exists it is what the developer is working on, and quietly grading
# their run with a published copy instead would hide exactly the change they
# are trying to test.
if [ -f "$CORE_ROOT/$SCRIPT_REL" ]; then
    validate "$CORE_ROOT/$SCRIPT_REL" && exit 0
    die "sibling script failed API/checksum validation: $CORE_ROOT/$SCRIPT_REL
  Run 'chore siblings' to move it to the pinned tag, or update EXPECTED_SHA256
  here if core has legitimately changed."
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
        validate "$package_script" && exit 0
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
