#!/usr/bin/env bash
# guard: rust-clippy
# Block the commit if `cargo clippy` reports any warning. Only runs in a Cargo
# project (skips silently otherwise). BLOCKS (exit 1) on lint failures — clippy
# flags logic smells, not layout, so a human should look rather than have it
# auto-rewritten.
set -u
dir=$(cd "$(dirname "$0")/.." && pwd)   # .githooks/
# shellcheck source=../lib/common.sh
. "$dir/lib/common.sh"

gg_is_rust || exit 0
root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0

# Run via gg_cargo (the rustup shim), which honors rust-toolchain.toml so local
# clippy == CI clippy. rc=2 means no cargo at all → skip rather than block.
rc=0; ( cd "$root" && gg_cargo clippy --all-targets -- -D warnings ); rc=$?
if [ "$rc" = 2 ]; then exit 0; fi
if [ "$rc" != 0 ]; then
  echo "github-guard: clippy found issues above — fix them, or bypass once with: git commit --no-verify" >&2
  exit 1
fi
exit 0
