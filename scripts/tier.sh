#!/usr/bin/env bash
# tier.sh LABEL LOG-NAME MAX-LINES MAX-BYTES -- COMMAND [ARG...]
#
# One test tier, run QUIETLY and under a budget. The whole run goes to
# tmp/logs/<LOG-NAME>.log; a pass prints one verdict line naming the log, a
# failure prints the tail of it, and a run that passed but printed more than
# its budget fails with status 65.
#
# WHY THE BUDGET IS PART OF THE TASK and not a CI-only check: the reader who
# pays most for a noisy suite is the one running it locally, and a rule that
# only CI enforces is a rule the tree drifts away from between pull requests.
# See the harness README's "Output: quiet by default, --verbose on request".
#
# The budgets themselves are in chores.yml, next to the command each one
# bounds, and every one of them was MEASURED — see the table there. Raise one
# deliberately when a tier grows; a budget nobody can breach measures nothing.
#
# VERBOSE. `OUTPUT_BUDGET_VERBOSE=1`, or `--verbose`/`-v` in the chore
# invocation's CLI_ARGS (`chore test:oracle -- --verbose`), streams the run as
# it happens as well as logging it. It does NOT lift the budget: the log is the
# same size either way, and a tier that has outgrown its budget should say so
# whether or not anybody was watching.
#
# THE VARIABLE WAS `FLTH_VERBOSE` until the wrapper moved to rust-fs-core. The
# old name is not read by the canonical script, and setting it does nothing --
# it does not error, the run simply stays quiet.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# THE WRAPPER BELONGS TO rust-fs-core, not to this repository and not to the
# test harness. It used to be read straight out of ../fs-linux-test-harness,
# which put a second copy of it one sibling away from core's; measured on
# 2026-09-22 the family had drifted to three copies reached four ways, and
# the harness held the stalest of them. resolve-output-budget.sh finds core's
# copy -- sibling first, then the packaged Cargo dependency -- and verifies it
# by checksum and API version before handing back a path.
BUDGET="$(bash "$REPO/scripts/resolve-output-budget.sh")"

[ $# -ge 5 ] || { echo "tier.sh: usage: tier.sh LABEL LOG MAX-LINES MAX-BYTES -- CMD..." >&2; exit 2; }
LABEL="$1"; LOG_NAME="$2"; MAX_LINES="$3"; MAX_BYTES="$4"; shift 4
[ "${1:-}" = "--" ] && shift
[ $# -gt 0 ] || { echo "tier.sh: no command" >&2; exit 2; }

# `chore test:oracle -- --verbose` arrives as CLI_ARGS. output-budget.sh reads
# OUTPUT_BUDGET_VERBOSE itself, so mapping the flag onto it is all that is
# needed -- and it means the environment variable and the flag cannot disagree.
case " ${CLI_ARGS:-} " in
    *" --verbose "*|*" -v "*) export OUTPUT_BUDGET_VERBOSE=1 ;;
esac

exec "$BUDGET" \
    --log "$REPO/tmp/logs/$LOG_NAME.log" \
    --max-lines "$MAX_LINES" \
    --max-bytes "$MAX_BYTES" \
    --label "$LABEL" \
    -- "$@"
