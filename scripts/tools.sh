#!/usr/bin/env bash
#
# tools.sh — install and verify the host oracle tools (`chore tools`).
#
#   tools.sh           install what is missing, then verify every tool
#   tools.sh --check   verify only; exit 1 naming what is missing
#
# The oracle tests (`chore test:oracle`) run on the HOST: e2fsprogs'
# mke2fs builds images, debugfs reads content and metadata back, e2fsck
# checks consistency, dumpe2fs and tune2fs read the superblock. The script
# tests (`chore test:scripts`) search the tree with ripgrep. Tests never
# skip when one is missing — they fail and name this task — so this is the
# one place that knows how to get them.
#
#   Linux   apt-get (with sudo when not root). e2fsprogs is also what
#           the harness VM installs for the fixture builder, so host and
#           guest agree on what the tools are.
#   macOS   prints the Homebrew formula: e2fsprogs is keg-only there,
#           and the tests find it under $(brew --prefix e2fsprogs)
#           without it being on PATH.
#
# The VM the fixtures need is checked separately: `chore vm:host:check`.
set -euo pipefail

MODE=install
[ "${1:-}" = "--check" ] && MODE=check

# 1.47.0 is the first e2fsprogs that knows `orphan_file`, and the oracle
# tests turn it off by name (`-O ^orphan_file`, a feature this driver
# does not write); older releases refuse the option outright. It is also
# what Debian 12 and Ubuntu 24.04 ship.
MIN_E2FSPROGS=1.47.0
# tool:debian-package:homebrew-formula
TOOLS="mke2fs:e2fsprogs:e2fsprogs mkfs.ext4:e2fsprogs:e2fsprogs e2fsck:e2fsprogs:e2fsprogs
fsck.ext4:e2fsprogs:e2fsprogs debugfs:e2fsprogs:e2fsprogs dumpe2fs:e2fsprogs:e2fsprogs
tune2fs:e2fsprogs:e2fsprogs rg:ripgrep:ripgrep"

# Find a tool the way tests/common/mod.rs (oracle_tool) does.
find_tool() {
    local name="$1" d
    for d in ${PATH//:/ } /usr/sbin /sbin /usr/local/sbin \
        /opt/homebrew/opt/e2fsprogs/sbin /usr/local/opt/e2fsprogs/sbin; do
        [ -x "$d/$name" ] && { echo "$d/$name"; return 0; }
    done
    return 1
}

# The entries whose tool is missing.
missing() {
    local entry out=""
    for entry in $TOOLS; do
        find_tool "${entry%%:*}" >/dev/null || out="$out $entry"
    done
    echo "${out# }"
}

# The distinct package names (field 2 or 3) of some entries.
packages() {
    local field="$1" entry
    shift
    for entry in "$@"; do
        echo "$entry" | cut -d: -f"$field"
    done | sort -u | tr '\n' ' '
}

names() {
    local entry
    for entry in "$@"; do printf '%s ' "${entry%%:*}"; done
}

version_ge() {
    [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -1)" = "$2" ]
}

install_linux() {
    local sudo="" pkgs
    # shellcheck disable=SC2046  # one word per package
    pkgs="$(packages 2 $(missing))"
    [ "$(id -u)" -eq 0 ] || sudo="sudo"
    if ! command -v apt-get >/dev/null 2>&1; then
        echo "tools: no apt-get on this Linux host; install with its package manager: $pkgs(e2fsprogs >= $MIN_E2FSPROGS)" >&2
        exit 1
    fi
    echo "tools: installing with apt-get: $pkgs"
    # shellcheck disable=SC2086  # the package list splits into words
    $sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq $pkgs >/dev/null ||
        { $sudo apt-get update -qq && $sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq $pkgs >/dev/null; }
}

gap="$(missing)"
if [ -n "$gap" ] && [ "$MODE" = install ]; then
    case "$(uname -s)" in
        Linux) install_linux ;;
        Darwin)
            # shellcheck disable=SC2086
            echo "tools: missing on this Mac: $(names $gap)" >&2
            # shellcheck disable=SC2086
            echo "       brew install $(packages 3 $gap)" >&2
            echo "       (keg-only: the tests find it under \$(brew --prefix e2fsprogs)/sbin)" >&2
            exit 1
            ;;
        *) echo "tools: unsupported host $(uname -s)" >&2; exit 1 ;;
    esac
    gap="$(missing)"
fi

if [ -n "$gap" ]; then
    # shellcheck disable=SC2086
    echo "tools: missing: $(names $gap)— run 'chore tools'" >&2
    exit 1
fi

version="$("$(find_tool mke2fs)" -V 2>&1 | sed -n 's/^mke2fs \([0-9][0-9.]*\).*/\1/p' | head -1)"
if [ -z "$version" ] || ! version_ge "$version" "$MIN_E2FSPROGS"; then
    echo "tools: e2fsprogs ${version:-of unknown version} is older than $MIN_E2FSPROGS" >&2
    exit 1
fi
for entry in $TOOLS; do
    printf '  %-9s %s\n' "${entry%%:*}" "$(find_tool "${entry%%:*}")"
done
echo "tools: e2fsprogs $version, $(rg --version | head -1) — all present"
