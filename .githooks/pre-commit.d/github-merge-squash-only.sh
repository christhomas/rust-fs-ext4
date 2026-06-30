#!/usr/bin/env bash
# guard: github-merge-squash-only
# Ensure the GitHub repo allows only squash + rebase merges (no merge commits),
# so PRs always land linear. Owner-only, fail-open — NEVER blocks the commit.
set -u
dir=$(cd "$(dirname "$0")/.." && pwd)   # .githooks/
# shellcheck source=../lib/common.sh
. "$dir/lib/common.sh"

slug=$(gg_repo_slug); [ -n "$slug" ] || exit 0
gg_have_gh || { echo "github-guard: gh not installed/authed — skipping merge-settings check for $slug" >&2; exit 0; }
owner=${slug%%/*}
gg_user_owns "$owner" || exit 0

allow=$(gh api "repos/$slug" --jq '.allow_merge_commit' 2>/dev/null) || {
  echo "github-guard: couldn't read merge settings for $slug — skipping" >&2; exit 0; }

if [ "$allow" = "true" ]; then
  echo "github-guard: $slug allows merge commits — switching to squash+rebase only…" >&2
  if gh api -X PATCH "repos/$slug" \
       -F allow_merge_commit=false -F allow_squash_merge=true -F allow_rebase_merge=true \
       >/dev/null 2>&1; then
    echo "github-guard: $slug merge settings fixed ✓" >&2
  else
    echo "github-guard: PATCH failed for $slug (need repo admin?) — not blocking" >&2
  fi
fi
exit 0
