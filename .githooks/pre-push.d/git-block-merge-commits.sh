#!/usr/bin/env bash
# guard: git-block-merge-commits
# Block any push whose pushed range contains a merge commit — the safety net
# behind git-block-merge-commit (also catches merges that arrived via fetch,
# cherry-pick, or a merge-preserving rebase). Reads the ref list git provides
# on stdin. Purely local. Hard block. Bypass only with --no-verify.
set -u
status=0
while read -r local_ref local_sha remote_ref remote_sha; do
  # Deleting a remote branch (local_sha all-zero): nothing to inspect.
  printf '%s' "$local_sha" | grep -qE '^0+$' && continue
  if printf '%s' "$remote_sha" | grep -qE '^0+$'; then
    # New branch on the remote: inspect commits not already on any remote ref.
    merges=$(git rev-list --merges "$local_sha" --not --remotes 2>/dev/null)
  else
    merges=$(git rev-list --merges "${remote_sha}..${local_sha}" 2>/dev/null)
  fi
  if [ -n "$merges" ]; then
    printf 'github-guard: BLOCKED — push to %s contains merge commit(s):\n' "$remote_ref" >&2
    printf '%s\n' "$merges" | sed 's/^/  /' >&2
    printf '  Linearize first:  git pull --rebase   |   git rebase <upstream>\n' >&2
    status=1
  fi
done
exit $status
