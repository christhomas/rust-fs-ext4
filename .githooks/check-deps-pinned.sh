#!/usr/bin/env bash
#
# Pre-commit gate: REFUSE to commit unpinned dependencies.
#
# A versioned tag must be reproducible — every dependency pinned to a
# version, the lockfile committed and in sync. This blocks the two ways
# that breaks:
#   1. A sibling crate fetched from a FLOATING BRANCH in a workflow
#      (`git clone` with no `--branch`, or `actions/checkout` with no
#      `ref:`). Floating = the published gate builds against whatever
#      HEAD happens to be — not reproducible.
#   2. A missing Cargo.lock, or a Cargo.lock whose own package version
#      has drifted from Cargo.toml (the classic "bumped the version but
#      forgot to re-lock", which only blows up later at `cargo publish`).
#
# It deliberately checks only what is reliable WITHOUT the path-dep
# siblings present (so it never false-blocks a fresh clone). CI's
# `cargo … --locked` is the full backstop.
#
# Bypass once (NOT recommended): git commit --no-verify
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
fail=0

# ── 1. no floating `git clone` of a sibling crate in any workflow ─────────────
shopt -s nullglob
for wf in .github/workflows/*.yml .github/workflows/*.yaml; do
    while IFS= read -r line; do
        case "$line" in
            *"git clone"*"github.com"*"/rust-"*)
                case "$line" in
                    *"--branch"*) : ;;                       # pinned → ok
                    *)
                        echo "[deps] FLOATING git clone (add --branch v<X>) in $wf:"
                        echo "       ${line#"${line%%[![:space:]]*}"}"
                        fail=1 ;;
                esac ;;
        esac
    done < "$wf"
done

# ── 2. no floating `actions/checkout` of a sibling crate (ruby = stdlib YAML) ──
if command -v ruby >/dev/null 2>&1; then
    ruby -ryaml -e '
        bad=[]
        Dir.glob(".github/workflows/*.{yml,yaml}").each do |wf|
            doc = (YAML.safe_load(File.read(wf), aliases: true) rescue nil)
            next unless doc.is_a?(Hash)
            (doc["jobs"]||{}).each_value do |job|
                next unless job.is_a?(Hash)
                (job["steps"]||[]).each do |st|
                    next unless st.is_a?(Hash)
                    next unless st["uses"].to_s.start_with?("actions/checkout")
                    w = st["with"] || {}
                    repo = w["repository"].to_s
                    if repo =~ %r{/rust-(fs-|partitions|img-)} && w["ref"].to_s.empty?
                        bad << "#{wf}: actions/checkout #{repo} has no ref: (pin to a tag)"
                    end
                end
            end
        end
        unless bad.empty?
            STDERR.puts "[deps] FLOATING actions/checkout (add ref: v<X>):"
            bad.each { |b| STDERR.puts "       #{b}" }
            exit 1
        end
    ' || fail=1
fi

# ── 3. Cargo.lock committed and self-consistent with Cargo.toml ───────────────
if [ -f Cargo.toml ]; then
    if [ ! -f Cargo.lock ]; then
        echo "[deps] Cargo.lock is not committed."
        echo "       Run: cargo generate-lockfile && git add Cargo.lock"
        fail=1
    else
        pkg=$(awk -F'"' '/^name[[:space:]]*=/{print $2; exit}' Cargo.toml)
        ver=$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' Cargo.toml)
        lockver=$(awk -v p="$pkg" '
            $0 == "name = \"" p "\"" { hit=1; next }
            hit && /^version = / { gsub(/^version = "|"$/, "", $0); print; exit }
        ' Cargo.lock)
        if [ -n "$pkg" ] && [ "$lockver" != "$ver" ]; then
            echo "[deps] Cargo.lock records $pkg = ${lockver:-<absent>} but Cargo.toml is $ver."
            echo "       The lock drifted from the manifest. Run: cargo generate-lockfile && git add Cargo.lock"
            fail=1
        fi
    fi
fi

# ── 4. authoritative stale-lock check (cargo is the oracle), best-effort ──────
# `cargo metadata --locked` refuses to rewrite the lock and errors if it is out
# of date with Cargo.toml — catching staleness the cheap version check above
# can't (e.g. a dependency added or version-bumped in Cargo.toml but never
# re-locked). Run it --offline so the hook stays fast and never hangs on the
# network. We only BLOCK when cargo says the lock needs updating; if cargo can't
# resolve for an unrelated reason (a path-dep sibling not checked out on a fresh
# clone, or an empty offline cache) we SKIP rather than false-block — CI's
# `--locked` is the final backstop.
if [ -f Cargo.toml ] && [ -f Cargo.lock ] && command -v cargo >/dev/null 2>&1; then
    if ! err=$(cargo metadata --locked --offline --format-version 1 2>&1 >/dev/null); then
        # cargo's staleness signal under --locked is "cannot update the lock
        # file ... because --locked was passed" (older cargo: "needs to be
        # updated"). Anything else (missing path-dep sibling, empty offline
        # cache) is an environment limitation, not staleness → skip.
        if printf '%s\n' "$err" | grep -qiE 'cannot update the lock file|needs to be updated|out.?of.?date'; then
            echo "[deps] Cargo.lock is STALE — it no longer matches Cargo.toml:"
            printf '%s\n' "$err" | grep -iE 'cannot update the lock file|needs to be updated|out.?of.?date' | head -1 | sed 's/^/       /'
            echo "       Fix: cargo generate-lockfile && git add Cargo.lock"
            fail=1
        else
            echo "[deps] note: deep lock-freshness check skipped (path-dep sibling or"
            echo "       registry cache unavailable offline); CI --locked is the backstop."
        fi
    fi
fi

if [ "$fail" != 0 ]; then
    echo "[deps] commit blocked — pin your dependencies (see above). Bypass once: git commit --no-verify"
    exit 1
fi
exit 0
