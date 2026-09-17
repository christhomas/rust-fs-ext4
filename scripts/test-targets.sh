#!/usr/bin/env bash
#
# test-targets.sh unit|oracle — print the `cargo test` target arguments
# for one tier of the suite (`chore test:unit`, `chore test:oracle`).
#
#   unit    the library, the binaries, and every tests/*.rs that reaches
#           neither a fixture nor an oracle tool
#   oracle  every tests/*.rs that runs an oracle tool
#
# DERIVED FROM THE TESTS THEMSELVES, not from a list someone has to keep:
# a test names a fixture by its test-disks/ path or through
# fs_ext4_test_support::fixture, and reaches a tool only through
# oracle_tool / assert_e2fsck_clean (the helpers that fail, never skip,
# when either is missing). tests/test_contract.rs fails if a test runs an
# e2fsprogs tool any other way, so the classification cannot drift.
#
# Library tests that need a fixture or a tool live in modules named
# `needs_host`; `unit` excludes them with cargo's name filter, and
# `chore test` runs everything.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
HOST='test-disks|fixture\(|oracle_tool\(|assert_e2fsck_clean\('
TOOL='oracle_tool\(|assert_e2fsck_clean\('

tier="${1:-}"
args=()
for f in "$REPO"/tests/*.rs; do
    name="$(basename "$f" .rs)"
    case "$tier" in
        unit) grep -qE "$HOST" "$f" || args+=(--test "$name") ;;
        oracle) grep -qE "$TOOL" "$f" && args+=(--test "$name") ;;
        *) echo "usage: test-targets.sh unit|oracle" >&2; exit 2 ;;
    esac
done
case "$tier" in
    unit) printf '%s\n' --lib --bins "${args[@]}" -- --skip needs_host:: ;;
    oracle) printf '%s\n' "${args[@]}" ;;
esac
