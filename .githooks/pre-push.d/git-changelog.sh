#!/usr/bin/env bash
# guard: git-changelog (pre-push)
# When you push a version tag (refs/tags/vN.N.N…), require the release to be
# documented. If the repo keeps a changelog — a root CHANGELOG.md and/or a
# "Changelog" section in README.md — then for the tag being pushed:
#   1. CHANGELOG.md must have a section for that version (if CHANGELOG.md exists),
#   2. the README changelog section must have one too (if that section exists),
#   3. the README changelog must list <= 10 versions (older live in CHANGELOG.md),
#   4. the README changelog must link to CHANGELOG.md.
# It reads the *tagged commit's* files, so it checks what actually ships. Blocks
# the push on violation. Self-gates: no version tag, or no changelog anywhere = no-op.
#
# Conventions: the README changelog section is a heading matching
# changelog/release notes/recent changes/releases/history; version entries
# inside it are deeper (e.g. `### v0.2.0`) sub-headings. Bypass: git push --no-verify.
#
# Also usable as a CLI: `git-changelog.sh notes vX.Y.Z` prints that version's
# release notes from CHANGELOG.md. A release pipeline can call this, so the
# changelog this guard enforces is the single source of truth for release bodies.
set -u
dir=$(cd "$(dirname "$0")/.." && pwd)   # .githooks/
# shellcheck source=../lib/common.sh
. "$dir/lib/common.sh"

# --- changelog extraction (shared by the hook below and a release pipeline) ---
# Print CHANGELOG.md's body for a version (no heading) — from its "## vX.Y.Z"
# heading up to the next "## v" heading, with leading blank lines trimmed.
# Reads the changelog on stdin.
changelog_section() {  # $1 = version, no leading v
  awk -v ver="$1" '
    /^##[[:space:]]+v/ {
      s=$0; sub(/^##[[:space:]]+v/, "", s); split(s, a, /[[:space:]]/); v=a[1]
      if (insec) exit
      if (v == ver) { insec=1; next }
    }
    insec {
      if (!started && $0 ~ /^[[:space:]]*$/) next
      started=1; print
    }
  '
}

# Print the version of the next-older release (the "## v" heading after $1),
# for the "Full Changelog" compare link. Empty for the first release.
changelog_prev_version() {  # $1 = version, no leading v
  awk -v ver="$1" '
    /^##[[:space:]]+v/ {
      s=$0; sub(/^##[[:space:]]+v/, "", s); split(s, a, /[[:space:]]/); v=a[1]
      if (found) { print v; exit }
      if (v == ver) found=1
    }
  '
}

# Subcommand `notes <version>`: emit GitHub release notes for a version from the
# SAME CHANGELOG.md this guard enforces, so the release body can't drift from the
# changelog. Fails loudly if the section is missing (the guard would have blocked
# the tag anyway).  Usage: git-changelog.sh notes v0.4.0
if [ "${1:-}" = "notes" ]; then
  ver=${2#v}
  root=$(git rev-parse --show-toplevel 2>/dev/null) || { echo "git-changelog: not a git repo" >&2; exit 1; }
  cl="$root/CHANGELOG.md"
  [ -f "$cl" ] || { echo "git-changelog: no CHANGELOG.md" >&2; exit 1; }
  body=$(changelog_section "$ver" < "$cl")
  [ -n "$body" ] || { echo "git-changelog: no '## v$ver' section in CHANGELOG.md" >&2; exit 1; }
  printf "## What's Changed\n\n%s\n" "$body"
  prev=$(changelog_prev_version "$ver" < "$cl")
  slug=$(gg_repo_slug)
  if [ -n "$prev" ] && [ -n "$slug" ]; then
    printf '\n**Full Changelog**: https://github.com/%s/compare/v%s...v%s\n' "$slug" "$prev" "$ver"
  fi
  exit 0
fi

# Self-gate: repos without a changelog convention are unaffected.
gg_has_changelog || exit 0

# Extract the README "changelog" section: everything after its heading up to the
# next level-2 (`## `) heading. (Version entries inside are `### …`, which don't
# end it.)
readme_changelog_section() {
  awk '
    /^## / {
      low = tolower($0)
      if (low ~ /change ?log|release notes|recent changes|releases|history/) { insec=1; next }
      else if (insec) { insec=0 }
    }
    insec { print }
  '
}

# True if any heading line in stdin names this version (tag or bare version,
# with digit boundaries so 0.2.0 doesn't match 10.2.0).
heading_has_version() {  # $1 = tag, $2 = ver-regex (dots escaped)
  grep -qE "^#{1,6}[[:space:]].*((^|[^0-9.])$2([^0-9]|\$)|$1)"
}

status=0
while read -r local_ref local_sha _remote_ref _remote_sha; do
  case "$local_ref" in refs/tags/v[0-9]*) ;; *) continue ;; esac     # version tags only
  printf '%s' "$local_sha" | grep -qE '^0+$' && continue              # tag deletion

  tag=${local_ref#refs/tags/}
  ver=${tag#v}
  ver_re=$(printf '%s' "$ver" | sed 's/\./\\./g')

  changelog=$(git show "$local_sha:CHANGELOG.md" 2>/dev/null || true)
  readme=$(git show "$local_sha:README.md" 2>/dev/null || true)
  readme_cl=$(printf '%s\n' "$readme" | readme_changelog_section)

  have_cl=0; [ -n "$changelog" ] && have_cl=1
  have_rcl=0; [ -n "$readme_cl" ] && have_rcl=1
  [ "$have_cl" = 1 ] || [ "$have_rcl" = 1 ] || continue               # no changelog convention → skip

  if [ "$have_cl" = 1 ] && ! printf '%s\n' "$changelog" | heading_has_version "$tag" "$ver_re"; then
    echo "git-changelog: CHANGELOG.md has no section for $tag." >&2; status=1
  fi
  if [ "$have_rcl" = 1 ]; then
    if ! printf '%s\n' "$readme_cl" | heading_has_version "$tag" "$ver_re"; then
      echo "git-changelog: README changelog has no section for $tag." >&2; status=1
    fi
    n=$(printf '%s\n' "$readme_cl" | grep -cE '^#{1,6}[[:space:]].*[0-9]+\.[0-9]+' || true)
    if [ "$n" -gt 10 ]; then
      echo "git-changelog: README changelog lists $n versions (max 10) — trim the oldest; full history stays in CHANGELOG.md." >&2; status=1
    fi
    if [ "$have_cl" = 1 ] && ! printf '%s\n' "$readme_cl" | grep -qi 'CHANGELOG\.md'; then
      echo "git-changelog: README changelog must link to CHANGELOG.md." >&2; status=1
    fi
  fi
done

if [ "$status" != 0 ]; then
  echo "  document the release then re-tag, or bypass once: git push --no-verify" >&2
fi
exit "$status"
