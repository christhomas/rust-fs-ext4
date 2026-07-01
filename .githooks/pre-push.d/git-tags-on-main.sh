#!/usr/bin/env bash
# guard: git-tags-on-main
# Block pushing any tag whose target commit is not contained in the default
# branch (main). Release tags must mark a commit that actually landed on main,
# never one stranded on a feature branch or a pre-squash line. Reads the ref
# list git provides on stdin. Purely local. Hard block. Bypass with --no-verify.
#
# Git has no hook for `git tag` creation (only reference-transaction, which we
# don't manage), so the push is the enforcement point — a tag only matters once
# it is shared. Annotated tags are peeled to their commit before the check.
set -u

# The remote being pushed to is the hook's first arg; resolve the default
# branch against THAT remote rather than a hardcoded "origin", so pushing a
# tag to an alternate remote still validates against the right branch.
remote="${1:-origin}"

# Resolve the default branch ($remote/HEAD -> main fallback) and collect every
# tip that represents it: the remote-tracking ref and the local branch. A tag
# commit reachable from EITHER counts as on-main, so a momentarily-stale
# remote-tracking ref can't cause a false block.
default=$(git symbolic-ref --quiet --short "refs/remotes/$remote/HEAD" 2>/dev/null)
default=${default#"$remote"/}
[ -n "$default" ] || default=main

mains=""
for ref in "refs/remotes/$remote/$default" "refs/heads/$default"; do
  sha=$(git rev-parse --quiet --verify "$ref" 2>/dev/null) && mains="$mains $sha"
done
# Can't locate main at all (no clone of it locally) — fail open, never block.
[ -n "$mains" ] || {
  echo "github-guard: can't resolve '$default' locally — skipping tag-on-main check" >&2
  exit 0
}

status=0
while read -r local_ref local_sha remote_ref remote_sha; do
  case "$local_ref" in refs/tags/*) ;; *) continue ;; esac
  # Tag deletion (local_sha all-zero): nothing to inspect.
  printf '%s' "$local_sha" | grep -qE '^0+$' && continue

  # Peel annotated/lightweight tag to the commit it ultimately points at. A tag
  # that doesn't peel to a commit (points at a tree/blob) can never be on the
  # default branch — block it rather than silently letting it through.
  commit=$(git rev-parse --quiet --verify "${local_sha}^{commit}" 2>/dev/null) || {
    tag=${remote_ref#refs/tags/}
    printf 'github-guard: BLOCKED — tag %s does not point at a commit object.\n' "$tag" >&2
    status=1
    continue
  }

  on_main=0
  for m in $mains; do
    if git merge-base --is-ancestor "$commit" "$m" 2>/dev/null; then on_main=1; break; fi
  done

  if [ "$on_main" != 1 ]; then
    tag=${remote_ref#refs/tags/}
    printf 'github-guard: BLOCKED — tag %s points at %s, which is not on %s.\n' \
      "$tag" "$(git rev-parse --short "$commit")" "$default" >&2
    printf '  Release tags must mark a commit that landed on %s. Re-point it:\n' "$default" >&2
    printf "    git tag -f '%s' <commit-on-%s>   # e.g. the squash-merge commit\n" "$tag" "$default" >&2
    printf "    git push --force '%s' '%s'\n" "$remote" "$tag" >&2
    status=1
  fi
done
exit $status
