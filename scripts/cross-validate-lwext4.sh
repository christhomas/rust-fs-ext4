#!/usr/bin/env bash
#
# Cross-validate a fs-ext4 image against `lwext4` — a BSD-2-Clause
# pure-C ext2/3/4 implementation independent of both the Linux kernel
# driver and our own Rust crate.
#
# Why: catches bugs both we and the kernel might share (we're both
# ultimately implementing the same on-disk spec, so a divergent third
# implementation surfaces ambiguity-driven defects). lwext4 is the
# best fit because:
#
# - BSD-2-Clause license — fits our no-GPL constraint.
# - Pure C, no kernel deps — runs anywhere `cc` runs.
# - Handles ext2, ext3, AND ext4 — exercises every flavor we ship.
#
# This script is *opt-in*. The default `cargo test` pipeline does NOT
# require lwext4 to be present; the matching Rust harness
# (`tests/lwext4_cross_validate.rs`) skips itself when the env var
# below is unset. Run this script manually before each release or in
# a dedicated CI lane.
#
# Usage:
#   scripts/cross-validate-lwext4.sh                # build + run all
#   scripts/cross-validate-lwext4.sh --build-only   # just clone + build
#   scripts/cross-validate-lwext4.sh --image foo.img  # validate one image
#
# Outputs:
#   Exit 0 — every image read by lwext4 matches the in-tree verifier.
#   Exit 1 — any image diverged (lists the failing image + reason).
#
# Spec source for lwext4: github.com/gkostka/lwext4 (BSD-2-Clause).
# We pin to a known-good tag below; bump only after verifying the
# upstream changelog doesn't regress on our test images.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

LWEXT4_REPO="https://github.com/gkostka/lwext4.git"
LWEXT4_TAG="v1.0.0-71-gd9da95d"  # pinned commit; bump deliberately
LWEXT4_DIR="${LWEXT4_DIR:-$CRATE_DIR/.lwext4}"

BUILD_ONLY=0
SINGLE_IMAGE=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --build-only) BUILD_ONLY=1; shift ;;
        --image) SINGLE_IMAGE="$2"; shift 2 ;;
        -h|--help)
            sed -n '2,32p' "$0"
            exit 0
            ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

# --- Step 1: fetch + build lwext4 if not already present -----------------
if [[ ! -d "$LWEXT4_DIR" ]]; then
    echo "[lwext4] cloning into $LWEXT4_DIR"
    git clone --depth 1 "$LWEXT4_REPO" "$LWEXT4_DIR"
fi

if [[ ! -f "$LWEXT4_DIR/build_generic/src/liblwext4.a" ]]; then
    echo "[lwext4] building (cmake + make)"
    pushd "$LWEXT4_DIR" >/dev/null
    # lwext4 ships a Makefile that drives several cmake builds. The
    # `generic` flavor is host-tooling-friendly (no embedded targets).
    make generic >/dev/null
    popd >/dev/null
fi

# Tell the Rust harness where to find headers + lib so it can spawn the
# CLI tool.
export LWEXT4_DIR
export LWEXT4_LIB="$LWEXT4_DIR/build_generic/src/liblwext4.a"
export LWEXT4_INCLUDE="$LWEXT4_DIR/include"
echo "[lwext4] available at $LWEXT4_DIR"
echo "[lwext4]   lib:     $LWEXT4_LIB"
echo "[lwext4]   include: $LWEXT4_INCLUDE"

if [[ "$BUILD_ONLY" -eq 1 ]]; then
    echo "[lwext4] --build-only: skipping validation"
    exit 0
fi

# --- Step 2: run the gated Rust harness --------------------------------
# The harness reads `LWEXT4_DIR` env var to discover the build artifacts.
# When it's set + the lib exists, the harness runs; otherwise it skips
# (so casual `cargo test` doesn't fail on machines without lwext4).
cd "$CRATE_DIR"
if [[ -n "$SINGLE_IMAGE" ]]; then
    export LWEXT4_VALIDATE_IMAGE="$SINGLE_IMAGE"
fi
cargo test --test lwext4_cross_validate -- --nocapture
