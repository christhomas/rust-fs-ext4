#!/usr/bin/env bash
# Host-side writable image paths must obey the test scratch policy. Literal
# /tmp paths bypass TMPDIR and put Raspberry Pi test churn on the SD card.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# ripgrep is REQUIRED. Every search below ends in `|| true` (no match is
# the passing case), so without this a missing `rg` printed "command not
# found", matched nothing and reported PASS -- which is how two violations
# sat in the tree while CI, whose runners have no ripgrep, stayed green.
if ! command -v rg >/dev/null 2>&1; then
    echo "FAIL  ripgrep (rg) is not installed; run 'chore tools'" >&2
    exit 1
fi
matches="$(rg -U -n 'format!\(\s*"/tmp/|"/tmp/(fs_ext4|rust-fs-ext4|ext4rs-)|std::env::temp_dir\(\)' \
    "$REPO/tests" "$REPO/src" \
    --glob '!**/support/src/lib.rs' \
    --glob '!**/test_temp_policy.rs' || true)"

selector_matches="$({
    rg -n '/tmp' "$REPO/scripts/test.sh" \
        | sed 's/\$REPO\/tmp//g' \
        | rg -n '/tmp'
    rg -n 'PathBuf::from\("/tmp|Path::new\("/tmp' \
        "$REPO/tests/support/src/lib.rs"
} || true)"

if [[ -n "$selector_matches" ]]; then
    matches="${matches:+$matches$'\n'}$selector_matches"
fi

if [[ -n "$matches" ]]; then
    echo "FAIL  writable host test paths bypass the scratch selector:" >&2
    printf '%s\n' "$matches" >&2
    exit 1
fi

echo "PASS  writable host test paths use the scratch selector"
