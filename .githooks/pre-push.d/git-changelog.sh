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
set -u
dir=$(cd "$(dirname "$0")/.." && pwd)   # .githooks/
# shellcheck source=../lib/common.sh
. "$dir/lib/common.sh"

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
