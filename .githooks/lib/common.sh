#!/usr/bin/env bash
# Shared helpers for github-guard guards. Source this file; it defines
# functions only and never exits the calling shell.
#
# Fail-open by design: a guard that blocks your work because gh/network/perms
# are unavailable is worse than the mistake it prevents. The local hard-block
# guards (merge commits) are the exception — they need no network.

# Echo the GitHub "owner/repo" slug for origin, or nothing if origin is missing
# or not on github.com.
gg_repo_slug() {
  local url
  url=$(git remote get-url origin 2>/dev/null) || return 0
  case "$url" in
    *github.com[:/]*) ;;
    *) return 0 ;;
  esac
  url=${url%.git}
  url=${url#*github.com[:/]}
  printf '%s' "$url"
}

# True if gh is installed and authenticated.
gg_have_gh() { command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; }

# Echo the authenticated GitHub login, or nothing.
gg_login() { gh api user --jq '.login' 2>/dev/null; }

# Return 0 only if the authenticated user OWNS this account: it is their
# personal account, or an org where their membership role is "admin" (owner).
# We change settings only on accounts we own — never other people's orgs, even
# where we happen to have repo-admin. A new org you create matches (you own it).
gg_user_owns() {
  local owner="$1" me role
  me=$(gg_login); [ -n "$me" ] || return 1
  [ "$owner" = "$me" ] && return 0
  role=$(gh api "user/memberships/orgs/$owner" --jq '.role' 2>/dev/null) || return 1
  [ "$role" = "admin" ]
}

# NOTE: deliberately no throttling. The network guards run only on commit/push
# — sparse, event-driven, a few calls each, nowhere near the 5000/hour API
# limit — so the ~1-2s they add to the occasional commit isn't worth a
# stamp-file/TTL mechanism.

# True if this repo keeps a changelog — a root CHANGELOG.md, or a "Changelog"
# (release notes / history) section in the root README.md. Guards that enforce
# changelog discipline self-gate on this: no changelog convention → they no-op,
# so projects without one are unaffected.
gg_has_changelog() {
  local root
  root=$(git rev-parse --show-toplevel 2>/dev/null) || return 1
  [ -f "$root/CHANGELOG.md" ] && return 0
  [ -f "$root/README.md" ] \
    && grep -qiE '^#{2,}[[:space:]]+(change ?log|release notes|recent changes|releases|history)\b' "$root/README.md" \
    && return 0
  return 1
}

# --- Rust helpers (shared by the rust-* guards) ------------------------------

# True if the repo root holds a Cargo.toml (i.e. it's a Cargo project).
gg_is_rust() {
  local root
  root=$(git rev-parse --show-toplevel 2>/dev/null) || return 1
  [ -f "$root/Cargo.toml" ]
}

# Run cargo via the rustup SHIM (`~/.cargo/bin/cargo`) so a repo's
# rust-toolchain.toml pin is honored automatically — local fmt/clippy then use
# the same toolchain as CI. A bare `cargo` can be Homebrew's, which ignores the
# pin entirely; the shim is the rustup proxy and respects it (installing the
# pinned toolchain on first use, as rustup intends). Falls back to whatever
# `cargo` is on PATH if the shim isn't present; returns 2 if there's no cargo
# at all (callers treat that as "skip, don't block").
gg_cargo() {
  local shim="$HOME/.cargo/bin/cargo"
  if [ -x "$shim" ]; then
    "$shim" "$@"
  elif command -v cargo >/dev/null 2>&1; then
    cargo "$@"
  else
    return 2
  fi
}
