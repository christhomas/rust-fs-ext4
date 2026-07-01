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

# Required status checks: auto-discover the default branch's GitHub Actions
# check-runs (so third-party app checks like coderabbit are excluded) and
# require them, strict. Discovery is scoped to the default branch's HEAD, so a
# newly-added job isn't required until it has run there once: protection lags a
# new job by one commit cycle (added on the first commit after its CI runs).
# Self-healing — re-applied whenever the discovered set drifts; never strips
# existing checks on a transient empty discovery.
#
# --paginate so repos with >30 distinct job names on HEAD aren't truncated to
# the first page; emit the modern `checks` shape ([{context}]) rather than the
# deprecated flat `contexts` array. Both `desired` (here) and `current` (the
# read-back below) end as compact JSON straight from jq's encoder — `desired`
# via `jq -c`, `current` via `tojson` — so escaping (quotes, backslashes) and
# sort order match and the equality check below is exact. gh streams the
# matching names (one per line) across all pages; jq slurps, wraps each as
# {context}, dedups + sorts. If jq is absent we leave `desired` empty and
# preserve whatever's already set (fail-open).
desired='[]'
if command -v jq >/dev/null 2>&1; then
  desired=$(gh api --paginate "repos/$slug/commits/$branch/check-runs?per_page=100" \
    --jq '.check_runs[] | select(.app.slug=="github-actions") | .name' 2>/dev/null \
    | jq -sRc 'split("\n") | map(select(length > 0)) | map({context: .}) | unique')
  [ -n "$desired" ] || desired='[]'
fi

# Current protection facts in one call: PR reviews present? admins enforced?
# plus the currently-required checks from the modern `checks` field (normalized
# to {context}, sorted). Each value is emitted on its OWN line, NOT through
# `@tsv` — `@tsv` adds a second escaping pass on top of `tojson`, so a job name
# containing `"` or `\` would read back double-escaped and never equal the
# `jq -c`-encoded `desired`, re-applying protection on every commit. `tojson`
# output is single-line, so line-reading each field is safe. Empty when unprotected.
{ IFS= read -r has_reviews; IFS= read -r has_admins; IFS= read -r current; } < <(
  gh api "repos/$slug/branches/$branch/protection" --jq \
    '(.required_pull_request_reviews != null),
     (.enforce_admins.enabled // false),
     ((.required_status_checks.checks // []) | map({context: .context}) | unique | tojson)' 2>/dev/null)
[ -n "$current" ] || current='[]'

# Checks to require: prefer a fresh discovery; else keep what's already set;
# never strip checks just because this commit's HEAD has no Actions runs yet.
if [ -n "$desired" ] && [ "$desired" != "[]" ]; then
  want="$desired"
elif [ -n "$current" ] && [ "$current" != "[]" ]; then
  want="$current"
else
  want="[]"
fi

# Already exactly how we want it (PR-mode + admins + matching checks)? Skip.
if [ "$has_reviews" = "true" ] && [ "$has_admins" = "true" ] && [ "${current:-[]}" = "$want" ]; then
  exit 0
fi

if [ "$want" = "[]" ]; then
  rsc='null'
  echo "github-guard: protecting $slug:$branch (require PR, enforce admins, linear history)…" >&2
else
  rsc="{ \"strict\": true, \"checks\": $want }"
  echo "github-guard: protecting $slug:$branch (require PR, enforce admins, linear history, required checks $want)…" >&2
fi

payload=$(cat <<JSON
{
  "required_status_checks": $rsc,
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
