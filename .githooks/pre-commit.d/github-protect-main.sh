#!/usr/bin/env bash
# guard: github-protect-main
# Ensure the repo's default branch is protected: require a PR (no direct
# pushes), enforced for admins too, linear history, no force-push/deletion —
# i.e. force everyone into PR mode. Owner-only, fail-open — NEVER blocks.
set -u
dir=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=../lib/common.sh
. "$dir/lib/common.sh"

slug=$(gg_repo_slug); [ -n "$slug" ] || exit 0
gg_have_gh || { echo "github-guard: gh not installed/authed — skipping branch-protection check for $slug" >&2; exit 0; }
owner=${slug%%/*}
gg_user_owns "$owner" || exit 0

branch=$(gh api "repos/$slug" --jq '.default_branch' 2>/dev/null) || {
  echo "github-guard: couldn't read default branch for $slug — skipping" >&2; exit 0; }
[ -n "$branch" ] || exit 0

# Already protected the way we want (require PR + enforced for admins)? Skip.
prot=$(gh api "repos/$slug/branches/$branch/protection" 2>/dev/null)
if printf '%s' "$prot" | grep -q '"required_pull_request_reviews"' \
   && printf '%s' "$prot" | grep -q '"enforce_admins"[^}]*"enabled":[[:space:]]*true'; then
  exit 0
fi

echo "github-guard: protecting $slug:$branch (require PR, enforce admins, linear history)…" >&2
payload=$(cat <<'JSON'
{
  "required_status_checks": null,
  "enforce_admins": true,
  "required_pull_request_reviews": { "required_approving_review_count": 0, "dismiss_stale_reviews": false, "require_code_owner_reviews": false },
  "restrictions": null,
  "required_linear_history": true,
  "allow_force_pushes": false,
  "allow_deletions": false
}
JSON
)
if printf '%s' "$payload" | gh api -X PUT "repos/$slug/branches/$branch/protection" \
     -H "Accept: application/vnd.github+json" --input - >/dev/null 2>&1; then
  echo "github-guard: $slug:$branch protected ✓" >&2
else
  echo "github-guard: protection PUT failed for $slug:$branch (need repo admin?) — not blocking" >&2
fi
exit 0
