#!/usr/bin/env bash
# Run tests in an owned scratch directory. On Raspberry Pi, keep write-heavy
# fixture copies on the checkout's storage rather than the system SD card.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_DIR=""

cleanup() {
    if [[ -n "$RUN_DIR" && -d "$RUN_DIR" ]]; then
        find "$RUN_DIR" -depth -mindepth 1 -delete
        rmdir "$RUN_DIR"
    fi
}
trap cleanup EXIT HUP INT TERM

if [[ -n "${FS_EXT4_TEST_TMPDIR:-}" ]]; then
    # An exact caller-supplied directory is not ours to delete.
    mkdir -p "$FS_EXT4_TEST_TMPDIR"
elif [[ -n "${FS_EXT4_TEST_TMP_BASE:-}" ]]; then
    mkdir -p "$FS_EXT4_TEST_TMP_BASE"
    RUN_DIR="$(mktemp -d "$FS_EXT4_TEST_TMP_BASE/fs-ext4-tests.XXXXXX")"
    export FS_EXT4_TEST_TMPDIR="$RUN_DIR"
elif [[ "${GITHUB_ACTIONS:-}" == "true" || "$(uname -s)" == "Darwin" ]]; then
    RUN_DIR="$(mktemp -d /tmp/fs-ext4-tests.XXXXXX)"
    export FS_EXT4_TEST_TMPDIR="$RUN_DIR"
elif [[ -r /proc/device-tree/model ]] && grep -aq 'Raspberry Pi' /proc/device-tree/model; then
    mkdir -p "$REPO/tmp"
    RUN_DIR="$(mktemp -d "$REPO/tmp/fs-ext4-tests.XXXXXX")"
    export FS_EXT4_TEST_TMPDIR="$RUN_DIR"
else
    RUN_DIR="$(mktemp -d "${TMPDIR:-/tmp}/fs-ext4-tests.XXXXXX")"
    export FS_EXT4_TEST_TMPDIR="$RUN_DIR"
fi

export TMPDIR="$FS_EXT4_TEST_TMPDIR"

if [[ "${1:-}" == "--print-temp-dir" ]]; then
    printf '%s\n' "$FS_EXT4_TEST_TMPDIR"
    exit 0
fi

cargo test "$@"
