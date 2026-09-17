#!/usr/bin/env bash
# scripts/test.sh's scratch policy, exercised as a caller sees it.
#
# ONE RULE: the scratch directory is inside this repository. The oracle
# tools run inside the fs-linux-test-harness VM, which sees this
# repository at the path the host knows it by and nothing else of the
# host — so an image under /tmp or $RUNNER_TEMP is a path the tool asked
# to read it cannot open. The Rust side of the same rule is
# `select_temp_dir` (tests/test_temp_policy.rs).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUTPUT="$REPO/tmp/runner-policy-output.txt"
EXACT="$REPO/tmp/runner-policy-exact"

cleanup() {
    rm -f "$OUTPUT"
    rmdir "$EXACT" 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

fail() { echo "FAIL  $1" >&2; exit 1; }

# 1. No environment at all: an owned directory under the repository's
#    tmp/, cleaned up on the way out.
env -u FS_EXT4_TEST_TMPDIR -u TMPDIR "$REPO/scripts/test.sh" --print-temp-dir > "$OUTPUT"
selected="$(cat "$OUTPUT")"
case "$selected" in
    "$REPO"/tmp/fs-ext4-tests.*) ;;
    *) fail "the default scratch directory is not inside the repository: $selected" ;;
esac
[[ -e "$selected" ]] && fail "runner did not clean its owned scratch directory: $selected"

# 2. An exact directory inside the repository is used as given, and is
#    the caller's to remove.
mkdir -p "$EXACT"
FS_EXT4_TEST_TMPDIR="$EXACT" "$REPO/scripts/test.sh" --print-temp-dir > "$OUTPUT"
[[ "$(cat "$OUTPUT")" == "$EXACT" ]] || fail "an exact FS_EXT4_TEST_TMPDIR was not used as given"
[[ -d "$EXACT" ]] || fail "the runner deleted a directory the caller supplied"

# 3. A directory outside the repository is REFUSED, and says why. This is
#    the case that used to be normal (/tmp, $RUNNER_TEMP) and now cannot
#    work: the guest would not find the image.
set +e
message="$(FS_EXT4_TEST_TMPDIR=/tmp/fs-ext4-outside "$REPO/scripts/test.sh" --print-temp-dir 2>&1)"
status=$?
set -e
[[ "$status" -ne 0 ]] || fail "a scratch directory outside the repository was accepted"
case "$message" in
    *"outside"*) ;;
    *) fail "the refusal does not say why: $message" ;;
esac

echo "PASS  the test runner keeps scratch inside the repository and cleans what it owns"
