#!/bin/bash
# Build ext4rs as a universal static library + xcframework for macOS.
#
# Outputs under dist/:
#   dist/libext4rs.a          — universal (arm64 + x86_64)
#   dist/ext4rs.xcframework   — Xcode-drop-in form
#   dist/ext4rs.h             — copy of the public header
#
# Intended for: CI release workflow, local developer builds, and Xcode
# Run-Script build phases that need libext4rs.a available before Swift
# compilation.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# Xcode runs scripts with a restricted PATH — discover cargo from login shell
CARGO_BIN=$(bash -l -c "command -v cargo" 2>/dev/null || true)
if [ -n "$CARGO_BIN" ]; then
    export PATH="$(dirname "$CARGO_BIN"):$PATH"
fi

if ! command -v cargo >/dev/null; then
    echo "ERROR: cargo not found. Install rustup and rerun." >&2
    exit 1
fi

# Profile: Release by default; Debug only when Xcode says so.
if [ "${CONFIGURATION:-Release}" = "Release" ]; then
    CARGO_FLAGS="--release"
    PROFILE_DIR="release"
else
    CARGO_FLAGS=""
    PROFILE_DIR="debug"
fi

# Ensure both darwin targets are installed (idempotent).
for TARGET in aarch64-apple-darwin x86_64-apple-darwin; do
    if ! rustup target list --installed 2>/dev/null | grep -q "^${TARGET}$"; then
        rustup target add "${TARGET}"
    fi
done

echo ">> Building ext4rs (${PROFILE_DIR}) for arm64 + x86_64..."
cargo build ${CARGO_FLAGS} --target aarch64-apple-darwin
cargo build ${CARGO_FLAGS} --target x86_64-apple-darwin

LIB=libext4rs.a
rm -rf dist
mkdir -p dist

echo ">> Lipoing universal static lib..."
lipo -create \
    "target/aarch64-apple-darwin/${PROFILE_DIR}/${LIB}" \
    "target/x86_64-apple-darwin/${PROFILE_DIR}/${LIB}" \
    -output "dist/${LIB}"
lipo -info "dist/${LIB}"

cp include/ext4rs.h dist/

echo ">> Assembling ext4rs.xcframework..."
# xcodebuild -create-xcframework rejects two libs for the same platform
# slice, so feed it the already-universal dist/libext4rs.a.
xcodebuild -create-xcframework \
    -library "dist/${LIB}" \
    -headers include \
    -output dist/ext4rs.xcframework >/dev/null

echo ">> Done. Artifacts in dist/:"
ls -lh dist/
