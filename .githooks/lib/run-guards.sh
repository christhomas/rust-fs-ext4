#!/usr/bin/env bash
# github-guard dispatcher (run-parts style). A hook stub invokes it as:
#
#   run-guards.sh <hook-name> [hook-args...]      (the hook's stdin is passed through)
#
# It runs every executable script in  <.githooks>/<hook-name>.d/  in lexical
# order, feeding each guard the original hook args and stdin. A guard that
# exits non-zero blocks the operation: github-guard names the offending guard
# and stops (later guards don't run). An empty/missing .d directory is a no-op,
# so a documented stub with no guards costs almost nothing.
set -u

hook=${1:-}
[ -n "$hook" ] && shift || { echo "github-guard: run-guards needs a hook name" >&2; exit 0; }

hooks_dir=$(cd "$(dirname "$0")/.." && pwd)
guard_dir="$hooks_dir/$hook.d"

# Fast no-op: nothing to run unless the .d dir holds at least one executable.
has_guard=0
if [ -d "$guard_dir" ]; then
  for g in "$guard_dir"/*; do
    if [ -f "$g" ] && [ -x "$g" ]; then has_guard=1; break; fi
  done
fi
[ "$has_guard" = 1 ] || exit 0

# Capture the hook's stdin once so every guard can read it (e.g. pre-push gets
# the ref list, post-rewrite gets the rewritten commits). Skip when stdin is a
# TTY (manual run) so we never hang waiting for EOF.
stdin_file=$(mktemp "${TMPDIR:-/tmp}/gg-stdin.XXXXXX")
trap 'rm -f "$stdin_file"' EXIT
if [ -t 0 ]; then : > "$stdin_file"; else cat > "$stdin_file"; fi

for guard in "$guard_dir"/*; do
  [ -f "$guard" ] && [ -x "$guard" ] || continue
  if ! "$guard" "$@" < "$stdin_file"; then
    echo "github-guard: '$(basename "$guard")' blocked '$hook'." >&2
    echo "             Fix the issue above, or bypass once with:  git ... --no-verify" >&2
    exit 1
  fi
done
exit 0
