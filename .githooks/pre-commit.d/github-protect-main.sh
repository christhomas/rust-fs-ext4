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

# Required status checks: auto-discover the checks that GATE A PULL REQUEST and
# require them, strict. The only checks that can gate a PR are the ones that run
# on `pull_request`, so discover them from recent pull_request workflow runs —
# NOT from the default branch's HEAD commit. That commit also carries release
# checks triggered by a tag push OR a `workflow_dispatch` on the branch; those
# never run on a PR, and requiring them makes every PR wait forever on checks
# that can't complete. Take the latest pull_request run per workflow and union
# their check-run names: exactly the PR gate. The github-actions app filter
# still excludes third-party checks like coderabbit. Self-healing — a renamed or
# added CI job syncs after the next PR runs it; never strips existing checks on
# a transient empty discovery. jq required; without it we preserve whatever's
# already set (fail-open).
#
# Both `desired` (here) and `current` (the read-back below) end as compact JSON
# straight from jq (`jq -c` / `tojson`), so escaping (quotes, backslashes) and
# sort order match and the equality check below is exact.
desired='[]'
if command -v jq >/dev/null 2>&1; then
  # check-suite of the latest pull_request run of each PR-triggering workflow.
  # per_page=100 (not 50) widens the window so a workflow whose latest PR run is
  # a bit older still gets discovered; the union below is the backstop for gaps.
  suite_ids=$(gh api "repos/$slug/actions/runs?event=pull_request&per_page=100" \
    --jq '[.workflow_runs[]?] | group_by(.workflow_id)[] | max_by(.created_at) | .check_suite_id' 2>/dev/null)
  desired=$(
    for sid in $suite_ids; do
      gh api --paginate "repos/$slug/check-suites/$sid/check-runs?per_page=100" \
        --jq '.check_runs[] | select(.app.slug=="github-actions") | .name' 2>/dev/null
    done | jq -sRc 'split("\n") | map(select(length > 0)) | map({context: .}) | unique'
  )
  # Empty stdin yields "[]" here, but guard against a stray "null" too so a
  # malformed value can never reach the protection PUT payload.
  case "$desired" in '' | null) desired='[]' ;; esac
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

# Checks to require: be strictly ADDITIVE — union what we just discovered with
# what's already required, never a bare replace. A discovery that's non-empty but
# PARTIAL (a PR-gating workflow whose latest run fell outside the window above)
# would otherwise overwrite `current` and silently drop the missing workflows'
# checks — the exact "never strip existing checks" promise, broken. Union keeps
# every already-required check and adds any newly-seen one. Empty discovery →
# keep current untouched. (jq required for the union; without it we already
# fell through with desired='[]' and keep current.)
if [ -n "$desired" ] && [ "$desired" != "[]" ]; then
  # $desired is only ever non-'[]' when the jq-guarded discovery above populated
  # it, so jq is guaranteed here — union discovered with existing (additive; never
  # a bare replace, which is what would drop checks on a partial discovery).
  want=$(printf '%s\n%s' "$desired" "$current" \
    | jq -sc 'add | map(select(.context? != null)) | unique')
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
