#!/usr/bin/env bash
# guard: git-block-bad-files
# Refuse to commit files that almost never belong in a repo and are costly to
# leak/track: private keys & certs, credential blobs, env files, OS junk, and
# merge cruft. Matches STAGED files by basename; blocks (exit 1) with an unstage
# hint.
#
# Conservative by design (safe to run in ANY repo): it does NOT match broad
# words like "secret"/"credential" — plenty of codebases legitimately have
# files named that way (e.g. a password manager). And env *templates*
# (.env.example/.sample/.template/.dist) are allowed. For a genuine edge,
# bypass once with `git commit --no-verify`.
set -uf   # -f: keep the '*' patterns literal during word-splitting

staged=$(git diff --cached --name-only --diff-filter=ACM)
[ -n "$staged" ] || exit 0

# Basename glob patterns that are (almost) never intended in a commit.
patterns='
.DS_Store
Thumbs.db
desktop.ini
*.pem
*.key
*.p12
*.pfx
*.keystore
*.jks
id_rsa
id_dsa
id_ecdsa
id_ed25519
*-adminsdk-*.json
service-account*.json
*.orig
*.rej
'

# .env and .env.<x> are secrets; the conventional templates are not.
env_is_secret() {
  case "$1" in
    .env) return 0 ;;
    .env.example|.env.sample|.env.template|.env.dist|.env.*.example) return 1 ;;
    .env.*) return 0 ;;
    *) return 1 ;;
  esac
}

fail=0
while IFS= read -r path; do
  [ -n "$path" ] || continue
  base=${path##*/}
  bad=0
  if env_is_secret "$base"; then
    bad=1
  else
    for pat in $patterns; do
      case "$base" in $pat) bad=1; break ;; esac
    done
  fi
  if [ "$bad" = 1 ]; then
    echo "github-guard: refusing to commit '$path' — looks like a key/credential/env/junk file." >&2
    echo "             unstage it:   git restore --staged \"$path\"" >&2
    echo "             or bypass once (intentional): git commit --no-verify" >&2
    fail=1
  fi
done <<EOF
$staged
EOF
exit "$fail"
