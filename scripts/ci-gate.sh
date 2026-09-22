#!/usr/bin/env bash
# Verify the one required check stands for every job.
#
# Two halves, and both must hold or the aggregate is decoration:
#
#   1. ci.yml    — the aggregate job `needs:` every other gating job, carries
#                  `if: always()`, and names no job that does not exist.
#   2. .github-guard — requires that aggregate and nothing else.
#
# WHY THIS IS A SCRIPT AND NOT A TEST. It parses a YAML file and compares
# strings; it exercises nothing this repository ships. As a `cargo test` it also
# counted towards the executed-test floor the gate itself enforces, so a repo
# could satisfy its floor partly by checking its own CI config.
#
# WHY IT IS NOT LINE-SCANNED. A quoted key, a flow mapping, and a `run: |` block
# whose CONTENTS look like a job key are all ordinary YAML that a line scan
# reads wrongly -- and a guard that misreads its input reports protection it is
# not providing. YAML 1.2 semantics matter too: GitHub's `on:` key must stay the
# string `on` rather than folding into a boolean, since that is the key telling
# a gating workflow from a release one.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOW="${CI_GATE_WORKFLOW:-.github/workflows/ci.yml}"
AGGREGATE="${CI_GATE_AGGREGATE:-ci-ok}"
GUARD="${CI_GATE_GUARD:-.github-guard}"
# Jobs exempt from gating: they carry `if:` or `continue-on-error:` and are
# deliberately advisory. Space separated. An exemption for a job that does not
# exist is an exemption waiting to silently cover a future job of that name.
NON_GATING="${CI_GATE_NON_GATING:-}"

cd "$ROOT" || exit 1
command -v python3 >/dev/null || { echo "ci-gate: python3 is required" >&2; exit 2; }

python3 - "$WORKFLOW" "$AGGREGATE" "$GUARD" "$NON_GATING" <<'PY'
import sys, re, os
wf, agg, guard, non_gating = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4].split()
fails = []

try:
    import yaml
except ImportError:
    print("ci-gate: python3 yaml module is required (pip install pyyaml)", file=sys.stderr)
    sys.exit(2)

if not os.path.exists(wf):
    print(f"ci-gate: {wf} is missing", file=sys.stderr); sys.exit(1)

# YAML 1.1 folds `on:` to True. Re-key it back so the trigger is readable.
doc = yaml.safe_load(open(wf)) or {}
triggers = doc.get("on", doc.get(True, {})) or {}
if isinstance(triggers, str):
    triggers = {triggers: None}
if isinstance(triggers, list):
    triggers = {t: None for t in triggers}

jobs = doc.get("jobs") or {}

if "pull_request" not in triggers:
    fails.append(
        f"`{wf}` does not run on `pull_request`, so nothing in it can gate a merge. "
        f"A required check that never reports reads to GitHub as permanently pending, "
        f"not failing. Triggers: {sorted(triggers)}")

if agg not in jobs:
    fails.append(
        f"`{wf}` has no `{agg}` job, so protection has to name every job by hand -- "
        f"and that list drifts the moment one is renamed, split or added. "
        f"Jobs present: {sorted(jobs)}")
else:
    body = jobs[agg] or {}
    needs = body.get("needs") or []
    if isinstance(needs, str):
        needs = [needs]

    cond = str(body.get("if", "")).strip()
    norm = cond.replace("${{", "").replace("}}", "").strip()
    if norm != "always()":
        if not cond:
            fails.append(
                f"`{agg}` does not carry `if: always()`, so a cancelled or skipped job "
                f"leaves it skipped too -- and a skipped required check never reports. "
                f"A job that was skipped is not a job that passed.")
        else:
            fails.append(
                f"`{agg}` carries `if: {cond}` rather than `if: always()`. Any condition "
                f"that can be false is a condition under which the one required check "
                f"does not report, and a required check that does not report reads as "
                f"permanently pending.")

    for n in needs:
        if n not in jobs:
            fails.append(f"`{agg}` needs `{n}`, which is not a job in `{wf}`.")

    for name in non_gating:
        if name not in jobs:
            fails.append(
                f"`{name}` is declared non-gating but is not a job in `{wf}`. An exemption "
                f"for a job that does not exist is an exemption waiting to silently cover "
                f"a future job of that name.")
            continue
        jb = jobs[name] or {}
        if "if" not in jb and "continue-on-error" not in jb:
            fails.append(
                f"`{name}` is declared non-gating but carries neither `if:` nor "
                f"`continue-on-error:`. It runs unconditionally and its failure is a real "
                f"failure, so exempting it takes a working gate off a working job.")

    for name, jb in jobs.items():
        if name == agg:
            continue
        jb = jb or {}
        conditional = ("if" in jb) or ("continue-on-error" in jb)
        declared = name in non_gating
        in_needs = name in needs
        if not conditional and not declared and not in_needs:
            fails.append(
                f"`{agg}` does not need `{name}`, so that job gates nothing: it can go red "
                f"and the merge still goes through. `{agg}` needs {needs}")
        if conditional and not declared and in_needs:
            fails.append(
                f"`{name}` carries a job-level `if:`/`continue-on-error:` and is in "
                f"`{agg}`'s needs without being declared non-gating. Decide which it is: "
                f"a skipped dependency is judged by `always()`, so an undeclared "
                f"conditional job silently changes what the gate means.")

# .github-guard: git-config format. `required =` inside a comment is prose.
required = []
if not os.path.exists(guard):
    fails.append(f"`{guard}` is missing, so nothing declares what protection should require.")
else:
    for line in open(guard):
        s = line.strip()
        if s.startswith("#") or s.startswith(";"):
            continue
        m = re.match(r'required\s*=\s*(.+)$', s)
        if m:
            val = m.group(1).strip().strip('"')
            required += [v for v in re.split(r'[,\s]+', val) if v]

if required != [agg]:
    fails.append(
        f"`{guard}` requires {required}; it should name `{agg}` alone. Requiring the "
        f"aggregate rather than each job means a job can be renamed, split or made "
        f"conditional without leaving a required check that never reports.")

if fails:
    print("ci-gate: the one required check does not stand for every job", file=sys.stderr)
    for f in fails:
        print(f"  - {f}", file=sys.stderr)
    sys.exit(1)

print(f"ci-gate: {guard} requires `{agg}`, and `{agg}` needs every job in {wf}")
PY
