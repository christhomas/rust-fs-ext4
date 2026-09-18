#!/usr/bin/env bash
# test-floor.sh TIER FLOOR  — the tier ran at least FLOOR tests
#
# THE FAILURE A BUDGET CANNOT SEE. scripts/tier.sh fails a tier that
# PRINTS more than it is allowed to; nothing fails a tier that prints
# almost nothing because it ran almost nothing. `cargo test` with a
# selection that matches no target exits 0, and a tier whose test file was
# renamed, or whose selection in scripts/test-targets.sh stopped matching
# it, is then a green line in the log saying `0 passed`.
#
# So a tier that exists to run ONE thing says how few tests still count as
# having run it. The number is MEASURED, like the budgets, and it only
# ever goes up: a floor lowered to make a run pass is a floor that has
# stopped measuring anything.
#
# It reads the tier's log (tmp/logs/<TIER>.log, written by tier.sh) rather
# than a pipe, so it cannot swallow the test run's own verdict — a
# `cargo test | test-floor.sh` would report this script's exit status and
# discard the suite's.
set -euo pipefail

[ $# -eq 2 ] || { echo "usage: test-floor.sh TIER FLOOR" >&2; exit 2; }
TIER="$1"
FLOOR="$2"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG="$REPO/tmp/logs/$TIER.log"

if [ ! -f "$LOG" ]; then
    echo "test-floor.sh: $LOG is missing -- the $TIER tier did not run." >&2
    exit 1
fi

# `test result: ok. 37 passed; 0 failed; ...`, one line per test binary.
ran="$(awk '/^test result: ok\./ { sum += $4 } END { print sum + 0 }' "$LOG")"
if [ "$ran" -lt "$FLOOR" ]; then
    echo "test-floor.sh: the $TIER tier executed $ran tests; the floor is $FLOOR." >&2
    echo "               A tier that runs fewer tests than it used to has stopped" >&2
    echo "               early rather than passed. Check scripts/test-targets.sh $TIER." >&2
    exit 1
fi
printf '%s\n' "$TIER: $ran tests executed (floor $FLOOR)"
