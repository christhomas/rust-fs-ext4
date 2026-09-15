#!/usr/bin/env bash
# Host-side writable image paths must obey the test scratch policy. Literal
# /tmp paths bypass TMPDIR and put Raspberry Pi test churn on the SD card.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
matches="$(rg -U -n 'format!\(\s*"/tmp/|"/tmp/(fs_ext4|rust-fs-ext4|ext4rs-)|std::env::temp_dir\(\)' \
    "$REPO/tests" "$REPO/src" \
    --glob '!**/support/src/lib.rs' \
    --glob '!**/test_temp_policy.rs' || true)"

if [[ -n "$matches" ]]; then
    echo "FAIL  writable host test paths bypass the scratch selector:" >&2
    printf '%s\n' "$matches" >&2
    exit 1
fi

echo "PASS  writable host test paths use the scratch selector"
