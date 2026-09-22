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

# THE WRAPPER IS COPIED FROM rust-fs-core FOR THIS RUN, AND DELETED AFTER IT.
#
# It belongs to core, and it is deliberately NOT committed here. A committed
# copy is a copy that drifts: measured on 2026-09-22 the family had three of
# them, reached four different ways, each repository internally consistent
# and nothing comparing them.
#
# CARGO IS ASKED WHERE CORE IS, rather than this script guessing. Cargo has
# already resolved the dependency, and its answer is right in both shapes
# this family uses: with `path = "../rust-fs-core"` it reports the developer's
# own checkout, so work in progress on the wrapper is exercised here on the
# next run; with a plain version requirement it reports the registry copy of
# the pinned release. There is no sibling-versus-crate decision to make,
# because cargo made it.
#
# tmp/ is gitignored and is where the tier logs already live.
CORE_DIR="$(cargo metadata --format-version 1 --locked --manifest-path "$REPO/Cargo.toml" \
    2>/dev/null | python3 -c '
import json, sys
packages = json.load(sys.stdin)["packages"]
print(next((p["manifest_path"].rsplit("/", 1)[0]
            for p in packages if p["name"] == "am-fs-core"), ""))
')"
if [ -z "$CORE_DIR" ] || [ ! -f "$CORE_DIR/scripts/output-budget.sh" ]; then
    echo "tier.sh: cargo could not say where am-fs-core is, or its copy has no" >&2
    echo "         scripts/output-budget.sh. The wrapper lives in rust-fs-core;" >&2
    echo "         check the am-fs-core dependency resolves and is at a version" >&2
    echo "         that ships it (v0.2.11 or later)." >&2
    exit 1
fi

BUDGET="$REPO/tmp/output-budget.$$.sh"
mkdir -p "$REPO/tmp"
cp "$CORE_DIR/scripts/output-budget.sh" "$BUDGET"
trap 'rm -f "$BUDGET"' EXIT

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

bash "$BUDGET" \
    --log "$REPO/tmp/logs/$LOG_NAME.log" \
    --max-lines "$MAX_LINES" \
    --max-bytes "$MAX_BYTES" \
    --label "$LABEL" \
    -- "$@"
