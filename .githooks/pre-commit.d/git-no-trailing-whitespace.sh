#!/usr/bin/env bash
# guard: git-no-trailing-whitespace
# Block the commit if the staged changes introduce whitespace errors — trailing
# whitespace, space-before-tab, or a stray blank line at EOF. Uses git's own
# `diff --cached --check`, which only flags lines THIS commit adds, so
# pre-existing whitespace in untouched lines never blocks your work.
set -u

if ! out=$(git diff --cached --check 2>&1); then
  echo "github-guard: staged changes introduce whitespace errors:" >&2
  printf '%s\n' "$out" | sed 's/^/  /' >&2
  echo "             trim them (most editors do on save), or bypass once: git commit --no-verify" >&2
  exit 1
fi
exit 0
