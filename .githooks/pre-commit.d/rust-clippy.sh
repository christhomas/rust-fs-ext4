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

# Fresh-clone safety: clippy must compile, which needs every `path = "…"`
# dependency present on disk. A crate whose path-dep sibling (e.g. ../foo) isn't
# checked out yet can't be linted — running clippy anyway would hard-block the
# commit on a missing sibling, not a real lint. Skip in that case (CI, which has
# every sibling, is the backstop). Scans all tracked Cargo.toml files; a path
# resolves against the manifest's own directory.
while IFS= read -r toml; do
  [ -f "$root/$toml" ] || continue
  tdir=$(cd "$(dirname "$root/$toml")" && pwd) || continue
  while IFS= read -r rel; do
    [ -n "$rel" ] || continue
    if [ ! -e "$tdir/$rel" ]; then
      echo "github-guard: rust-clippy skipped — path dependency '$rel' (in $toml) isn't" >&2
      echo "             checked out; clippy can't compile without it. CI enforces clippy." >&2
      exit 0
    fi
  done < <(grep -oE 'path[[:space:]]*=[[:space:]]*"[^"]+"' "$root/$toml" 2>/dev/null | sed -E 's/.*"([^"]+)".*/\1/')
done < <(git -C "$root" ls-files '*Cargo.toml' 'Cargo.toml')

# Run via gg_cargo (the rustup shim), which honors rust-toolchain.toml so local
# clippy == CI clippy. rc=2 means no cargo at all → skip rather than block.
rc=0; ( cd "$root" && gg_cargo clippy --all-targets -- -D warnings ); rc=$?
if [ "$rc" = 2 ]; then exit 0; fi
if [ "$rc" != 0 ]; then
  echo "github-guard: clippy found issues above — fix them, or bypass once with: git commit --no-verify" >&2
  exit 1
fi
exit 0
