#!/usr/bin/env bash
# guard: rust-deps-pinned
# Reproducible-release gate for Cargo projects: refuse a commit that would make
# a versioned tag non-reproducible. Only runs when Cargo.toml is present (skips
# silently otherwise), so non-Rust repos are unaffected. BLOCKS on a real
# problem; fail-OPEN whenever the environment can't give a reliable answer (no
# cargo, empty offline cache, a path-dep sibling not checked out) — CI's
# `cargo … --locked` is the authoritative backstop.
#
# It catches the ways a "pinned" release silently isn't:
#   1. A workflow that FLOATING-clones a sibling repo (same GitHub owner) with
#      no `--branch`/`-b` — the published build then resolves against whatever
#      that repo's HEAD happens to be, not a fixed tag.
#   2. A workflow `actions/checkout` of a sibling repo with no `ref:` — same
#      floating hazard, via the action instead of raw git.
#   3. A committed Cargo.lock whose own package version drifted from Cargo.toml
#      (bumped the manifest, forgot to re-lock — only blows up at `cargo publish`).
#   4. A Cargo.lock that `cargo metadata --locked` reports as stale.
#
# Bypass once (NOT recommended): git commit --no-verify
set -u
dir=$(cd "$(dirname "$0")/.." && pwd)   # .githooks/
# shellcheck source=../lib/common.sh
. "$dir/lib/common.sh"

gg_is_rust || exit 0
root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$root" || exit 0
fail=0

# ── 1 & 2. no FLOATING fetch of a sibling repo (same owner) in any workflow ──
# A "sibling" is another repo under the same GitHub owner as origin — those are
# the path/git dependency crates a release must pin. If the owner can't be
# determined (no github origin) we skip these two checks; the lock checks below
# still run.
owner=$(gg_repo_slug); owner=${owner%%/*}
if [ -n "$owner" ] && [ -d .github/workflows ]; then
  shopt -s nullglob 2>/dev/null || true
  for wf in .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$wf" ] || continue
    while IFS= read -r line; do
      case "$line" in
        *"git clone"*github.com[:/]"$owner"/*)
          case "$line" in
            *--branch*|*" -b "*) : ;;                      # pinned to a tag → ok
            *)
              echo "[deps] FLOATING git clone of a sibling repo (add --branch v<X>) in $wf:" >&2
              echo "       ${line#"${line%%[![:space:]]*}"}" >&2
              fail=1 ;;
          esac ;;
      esac
    done < "$wf"
  done

  # actions/checkout of an explicit sibling repository: with no ref:. Parsed
  # with ruby's stdlib YAML when available; skipped (never failed) if ruby isn't.
  if command -v ruby >/dev/null 2>&1; then
    ruby -ryaml -e '
      owner = ARGV[0]
      bad = []
      Dir.glob(".github/workflows/*.{yml,yaml}").each do |wf|
        doc = (YAML.safe_load(File.read(wf), aliases: true) rescue nil)
        next unless doc.is_a?(Hash)
        (doc["jobs"] || {}).each_value do |job|
          next unless job.is_a?(Hash)
          (job["steps"] || []).each do |st|
            next unless st.is_a?(Hash)
            next unless st["uses"].to_s.start_with?("actions/checkout")
            w = st["with"] || {}
            repo = w["repository"].to_s
            next if repo.empty?
            next unless repo.split("/").first == owner        # sibling only
            bad << "#{wf}: actions/checkout #{repo} has no ref: (pin to a tag)" if w["ref"].to_s.empty?
          end
        end
      end
      unless bad.empty?
        STDERR.puts "[deps] FLOATING actions/checkout of a sibling repo (add ref: v<X>):"
        bad.each { |b| STDERR.puts "       #{b}" }
        exit 1
      end
    ' "$owner" || fail=1
  fi
fi

# ── 3. Cargo.lock consistency — only when the repo actually commits a lock ───
# Respect the repo's own convention: enforce the lock if it's tracked; never
# invent one for a library that deliberately gitignores it.
lock_tracked=0; git ls-files --error-unmatch Cargo.lock >/dev/null 2>&1 && lock_tracked=1
if [ "$lock_tracked" = 1 ] && [ ! -f Cargo.lock ]; then
  echo "[deps] Cargo.lock is tracked but missing from the working tree." >&2
  echo "       Restore it: git checkout -- Cargo.lock   (or: cargo generate-lockfile)" >&2
  fail=1
elif [ -f Cargo.lock ]; then
  pkg=$(awk -F'"' '/^name[[:space:]]*=/{print $2; exit}' Cargo.toml)
  ver=$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' Cargo.toml)
  if [ -n "$pkg" ] && [ -n "$ver" ]; then
    lockver=$(awk -v p="$pkg" '
      $0 == "name = \"" p "\"" { hit=1; next }
      hit && /^version = / { gsub(/^version = "|"$/, "", $0); print; exit }
    ' Cargo.lock)
    if [ -n "$lockver" ] && [ "$lockver" != "$ver" ]; then
      echo "[deps] Cargo.lock records $pkg = $lockver but Cargo.toml is $ver — the lock" >&2
      echo "       drifted from the manifest. Run: cargo generate-lockfile && git add Cargo.lock" >&2
      fail=1
    fi
  fi

  # ── 4. authoritative stale-lock check (cargo is the oracle), best-effort ──
  # `cargo metadata --locked` refuses to rewrite the lock and errors if it's out
  # of date. Run --offline so the hook stays fast and never touches the network.
  #
  # BUT only when the graph resolves the same as CI's. A crate with an EXTERNAL
  # `path = "../sibling"` dependency can't guarantee that locally: a sibling
  # checked out at a version that differs from what the lock records (routine in
  # multi-repo dev) makes cargo want to re-lock, which surfaces as the exact same
  # "cannot update the lock file" error as true staleness — a false block we must
  # not raise. So skip part 4 whenever an external path dep is present; CI (with
  # siblings pinned to their tagged versions) is the authoritative --locked
  # backstop, and parts 1–3 above still apply. Registry-only / in-repo-workspace
  # crates keep the full check.
  ext_path_dep=0
  while IFS= read -r toml; do
    if grep -qE 'path[[:space:]]*=[[:space:]]*"\.\.?/' "$toml" 2>/dev/null; then ext_path_dep=1; break; fi
  done < <(git ls-files '*Cargo.toml' 'Cargo.toml')
  if [ "$ext_path_dep" = 0 ]; then
    # BLOCK only on the staleness signal; any other failure (empty offline cache,
    # etc.) is an environment limitation → skip. gg_cargo returns 2 with no cargo.
    if err=$(gg_cargo metadata --locked --offline --format-version 1 2>&1 >/dev/null); then
      : # lock is fresh
    else
      rc=$?
      if [ "$rc" != 2 ] && printf '%s\n' "$err" | grep -qiE 'cannot update the lock file|needs to be updated|out.?of.?date'; then
        echo "[deps] Cargo.lock is STALE — it no longer matches Cargo.toml:" >&2
        printf '%s\n' "$err" | grep -iE 'cannot update the lock file|needs to be updated|out.?of.?date' | head -1 | sed 's/^/       /' >&2
        echo "       Fix: cargo generate-lockfile && git add Cargo.lock" >&2
        fail=1
      fi
    fi
  fi
fi

if [ "$fail" != 0 ]; then
  echo "github-guard: rust-deps-pinned blocked the commit — pin your dependencies (above)." >&2
  echo "             Bypass once (NOT recommended): git commit --no-verify" >&2
  exit 1
fi
exit 0
