#!/usr/bin/env bash
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TEST_BASE="$REPO/tmp/runner-policy-test"
OUTPUT="$REPO/tmp/runner-policy-output.txt"

cleanup() {
    rm -f "$OUTPUT"
    rmdir "$TEST_BASE" 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$TEST_BASE"
FS_EXT4_TEST_TMP_BASE="$TEST_BASE" "$REPO/scripts/test.sh" --print-temp-dir > "$OUTPUT"
SELECTED="$(cat "$OUTPUT")"

case "$SELECTED" in
    "$TEST_BASE"/fs-ext4-tests.*) ;;
    *)
        echo "FAIL  selected scratch directory is outside the requested base: $SELECTED" >&2
        exit 1
        ;;
esac

if [[ -e "$SELECTED" ]]; then
    echo "FAIL  runner did not clean its owned scratch directory: $SELECTED" >&2
    exit 1
fi

FS_EXT4_TEST_TMPDIR= FS_EXT4_TEST_TMP_BASE= \
    GITHUB_ACTIONS=true RUNNER_TEMP="$TEST_BASE" TMPDIR=/must-not-be-used \
    "$REPO/scripts/test.sh" --print-temp-dir > "$OUTPUT"
SELECTED="$(cat "$OUTPUT")"

case "$SELECTED" in
    "$TEST_BASE"/fs-ext4-tests.*) ;;
    *)
        echo "FAIL  GitHub scratch directory is outside RUNNER_TEMP: $SELECTED" >&2
        exit 1
        ;;
esac

if [[ -e "$SELECTED" ]]; then
    echo "FAIL  runner did not clean its GitHub scratch directory: $SELECTED" >&2
    exit 1
fi

# A Raspberry Pi deliberately selects the worktree-backed NVMe path here, so
# exercise the platform-default fallback on the non-Pi hosted runners instead.
if [[ ! -r /proc/device-tree/model ]] || ! grep -aq 'Raspberry Pi' /proc/device-tree/model; then
    FS_EXT4_TEST_TMPDIR= FS_EXT4_TEST_TMP_BASE= \
        GITHUB_ACTIONS=false RUNNER_TEMP= env -u TMPDIR \
        "$REPO/scripts/test.sh" --print-temp-dir > "$OUTPUT"
    SELECTED="$(cat "$OUTPUT")"

    case "$(basename "$SELECTED")" in
        fs-ext4-tests.*) ;;
        *)
            echo "FAIL  platform fallback did not create an isolated scratch directory: $SELECTED" >&2
            exit 1
            ;;
    esac

    if [[ -e "$SELECTED" ]]; then
        echo "FAIL  runner did not clean its platform scratch directory: $SELECTED" >&2
        exit 1
    fi
fi

echo "PASS  test runner selects and cleans an owned scratch directory"
