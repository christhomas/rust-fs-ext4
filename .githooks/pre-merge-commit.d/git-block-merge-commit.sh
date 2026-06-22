#!/usr/bin/env bash
# guard: git-block-merge-commit
# Refuse to CREATE a merge commit locally, keeping history linear. This fires
# on a non-fast-forward `git merge` / `git pull`. Purely local, no network.
# Hard block (exit 1). Bypass only with git's built-in --no-verify.
printf 'github-guard: BLOCKED — this merge would create a merge commit.\n' >&2
printf '  Linearize instead:  git merge --ff-only   |   git pull --rebase   |   git rebase\n' >&2
exit 1
