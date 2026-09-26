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
# THE VARIABLE WAS `FLTH_VERBOSE` until the wrapper moved to rust-fs-core. A
# rename like that fails silently -- `--verbose` simply stops working and
# nothing errors -- so the canonical script now names the replacement on
# stderr when it sees an `FLTH_*` variable. It does not honour it.
#
# A FAILING TIER IS QUIET TOO, from core v0.2.13. It prints the verdict, the
# exit status and the log's path, not forty lines of tail; `--tail N` or
# OUTPUT_BUDGET_FAIL_TAIL=N brings the tail back for whoever is watching.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# THE WRAPPER IS COPIED FROM rust-fs-core FOR THIS RUN, AND DELETED AFTER IT.
#
# It belongs to core, and it is deliberately NOT committed here. A committed
# copy is a copy that drifts: measured on 2026-09-22 the family had three of
# them, reached four different ways, each repository internally consistent
# and nothing comparing them.
#
# THE SIBLING FIRST, THEN WHATEVER CARGO RESOLVED. The sibling comes first so
# a coordinated local change to core's wrapper is actually exercised on the
# next run here, rather than being masked by a registry copy of the last
# release. A standalone checkout with no sibling falls through to cargo, which
# has already resolved am-fs-core and can say where it put it.
#
# FS_CORE_ROOT NAMES CORE OUTRIGHT, and when it is set there is no fallback:
# you have said where core is, so its absence there is an answer, not a reason
# to go looking. It exists so both refusals below can be driven by
# tests/scripts/test-output-budget.sh -- a resolver whose failure path nobody
# executes has never been shown to refuse anything, which is the same defect
# as a test that skips.
#
# WHAT IS VERIFIED IS `--version`, NOT A DIGEST. rust-fs-ntfs pins the script's
# SHA-256, which means every comment core adds to it breaks a consumer until
# the digest is chased; the same pin in seven repositories is the lockstep this
# migration exists to remove. `rust-fs-core-output-budget 1` is the contract,
# and a copy that cannot print it is not core's.
#
# A WRONG COPY IS FATAL, NOT A REASON TO LOOK ELSEWHERE. Falling through to the
# next candidate would turn "core is broken" into "core is missing", which is a
# different and much quieter problem.
EXPECTED_API="rust-fs-core-output-budget 1"
CORE_ROOT="${FS_CORE_ROOT:-$REPO/../rust-fs-core}"

verified() {
    [ -f "$1" ] || return 1
    [ "$(bash "$1" --version 2>/dev/null || true)" = "$EXPECTED_API" ]
}

refuse_wrong() {
    echo "tier.sh: $1/scripts/output-budget.sh is there, but does not answer" >&2
    echo "         --version with '$EXPECTED_API'. That is a broken or far too" >&2
    echo "         old rust-fs-core, not an absent one, so this stops here" >&2
    echo "         rather than quietly looking somewhere else." >&2
    exit 1
}

CORE_SCRIPT=""
if [ -n "${FS_CORE_ROOT:-}" ]; then
    if [ -e "$CORE_ROOT/scripts/output-budget.sh" ]; then
        verified "$CORE_ROOT/scripts/output-budget.sh" || refuse_wrong "$CORE_ROOT"
    else
        echo "tier.sh: FS_CORE_ROOT names $CORE_ROOT, which has no" >&2
        echo "         scripts/output-budget.sh. The wrapper lives in" >&2
        echo "         rust-fs-core and is deliberately not committed here." >&2
        exit 1
    fi
    CORE_SCRIPT="$CORE_ROOT/scripts/output-budget.sh"
elif [ -e "$CORE_ROOT/scripts/output-budget.sh" ]; then
    verified "$CORE_ROOT/scripts/output-budget.sh" || refuse_wrong "$CORE_ROOT"
    CORE_SCRIPT="$CORE_ROOT/scripts/output-budget.sh"
else
    CORE_DIR="$(cargo metadata --format-version 1 --locked --manifest-path "$REPO/Cargo.toml" \
        2>/dev/null | python3 -c '
import json, sys
packages = json.load(sys.stdin)["packages"]
print(next((p["manifest_path"].rsplit("/", 1)[0]
            for p in packages if p["name"] == "am-fs-core"), ""))
' 2>/dev/null)"
    if [ -n "$CORE_DIR" ] && [ -e "$CORE_DIR/scripts/output-budget.sh" ]; then
        verified "$CORE_DIR/scripts/output-budget.sh" || refuse_wrong "$CORE_DIR"
        CORE_SCRIPT="$CORE_DIR/scripts/output-budget.sh"
    fi
fi

if [ -z "$CORE_SCRIPT" ]; then
    echo "tier.sh: no rust-fs-core supplied scripts/output-budget.sh." >&2
    echo "         The wrapper lives in rust-fs-core and is deliberately not" >&2
    echo "         committed here. Looked for the sibling at" >&2
    echo "           $CORE_ROOT/scripts/output-budget.sh" >&2
    echo "         then asked cargo for the am-fs-core package." >&2
    echo "         Run 'chore siblings', or depend on v0.2.13 or later." >&2
    exit 1
fi

# THE WRAPPER IS COPIED FOR THIS RUN AND DELETED AFTER IT, so a `chore siblings`
# or a `cargo update` part-way through a long tier cannot change the script out
# from under it. tmp/ is gitignored and is where the tier logs already live.

BUDGET="$REPO/tmp/output-budget.$$.sh"
mkdir -p "$REPO/tmp"
cp "$CORE_SCRIPT" "$BUDGET"
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
