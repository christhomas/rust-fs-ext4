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

# Reconcile the FULL desired state — no merge commits, squash + rebase allowed —
# whenever ANY of the three differ. Reading only allow_merge_commit missed the
# common case of a repo that already has merge commits off but still disallows
# squash (e.g. rebase-only): the old check saw merge_commit=false and skipped,
# leaving squash disabled so `gh pr merge --squash` failed.
set -- $(gh api "repos/$slug" \
  --jq '"\(.allow_merge_commit) \(.allow_squash_merge) \(.allow_rebase_merge)"' 2>/dev/null)
mc=${1:-} sq=${2:-} rb=${3:-}
# Only act on a well-formed response: each field must be a literal boolean. A
# missing field makes jq emit "null" (non-empty), and a failed/edge read yields
# empty — neither must be mistaken for "needs reconciling" and trigger a PATCH.
for v in "$mc" "$sq" "$rb"; do
  case "$v" in
    true|false) ;;
    *) echo "github-guard: unexpected merge-settings response for $slug — skipping" >&2; exit 0 ;;
  esac
done

if [ "$mc" != "false" ] || [ "$sq" != "true" ] || [ "$rb" != "true" ]; then
  echo "github-guard: $slug merge settings (merge=$mc squash=$sq rebase=$rb) — reconciling to squash+rebase only…" >&2
  if gh api -X PATCH "repos/$slug" \
       -F allow_merge_commit=false -F allow_squash_merge=true -F allow_rebase_merge=true \
       >/dev/null 2>&1; then
    echo "github-guard: $slug merge settings fixed ✓" >&2
  else
    echo "github-guard: PATCH failed for $slug (need repo admin?) — not blocking" >&2
  fi
fi
exit 0
