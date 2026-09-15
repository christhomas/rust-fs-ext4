#!/usr/bin/env bash
# Build external-tool fixtures directly on a Linux CI host. GitHub's standard
# ARM runner is already real Linux, so another Linux kernel under QEMU would
# add a nested-virtualisation dependency without improving the oracle.
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
    echo "native fixture generation requires Linux" >&2
    exit 2
fi
if [[ "$EUID" -ne 0 ]]; then
    echo "native fixture generation mounts disposable loop images and must run under sudo" >&2
    exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
OUTPUT_DIR="${EXT4_FIXTURE_OUTPUT_DIR:-$SCRIPT_DIR}"
MOUNT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ext4-fixture-mount.XXXXXX")"

cleanup() {
    umount "$MOUNT_DIR" 2>/dev/null || true
    rmdir "$MOUNT_DIR" 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$OUTPUT_DIR"
EXT4_MOUNT_DIR="$MOUNT_DIR" sh "$SCRIPT_DIR/_vm-builder.sh" "$OUTPUT_DIR" "$@"

if [[ -n "${SUDO_UID:-}" && -n "${SUDO_GID:-}" ]]; then
    find "$OUTPUT_DIR" -maxdepth 1 -type f -name 'ext4-*.img' \
        -exec chown "$SUDO_UID:$SUDO_GID" {} +
fi
