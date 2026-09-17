#!/usr/bin/env bash
#
# tools.sh — install and verify the host oracle tools (`chore tools`).
#
#   tools.sh           install what is missing, then verify every tool
#   tools.sh --check   verify only; exit 1 naming what is missing
#
# The oracle tests (`chore test:oracle`) run on the HOST: e2fsprogs'
# mke2fs builds images, debugfs reads content and metadata back, e2fsck
# checks consistency, dumpe2fs and tune2fs read the superblock. Tests
# never skip when one is missing — they fail and name this task — so
# this is the one place that knows how to get them.
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
TOOLS="mke2fs mkfs.ext4 e2fsck fsck.ext4 debugfs dumpe2fs tune2fs"

# Find a tool the way tests/common/mod.rs (oracle_tool) does.
find_tool() {
    local name="$1" d
    for d in ${PATH//:/ } /usr/sbin /sbin /usr/local/sbin \
        /opt/homebrew/opt/e2fsprogs/sbin /usr/local/opt/e2fsprogs/sbin; do
        [ -x "$d/$name" ] && { echo "$d/$name"; return 0; }
    done
    return 1
}

missing() {
    local t out=""
    for t in $TOOLS; do
        find_tool "$t" >/dev/null || out="$out $t"
    done
    echo "${out# }"
}

version_ge() {
    [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -1)" = "$2" ]
}

install_linux() {
    local sudo=""
    [ "$(id -u)" -eq 0 ] || sudo="sudo"
    if ! command -v apt-get >/dev/null 2>&1; then
        echo "tools: no apt-get on this Linux host; install e2fsprogs >= $MIN_E2FSPROGS with its package manager." >&2
        exit 1
    fi
    echo "tools: installing e2fsprogs with apt-get"
    $sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq e2fsprogs >/dev/null ||
        { $sudo apt-get update -qq && $sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq e2fsprogs >/dev/null; }
}

gap="$(missing)"
if [ -n "$gap" ] && [ "$MODE" = install ]; then
    case "$(uname -s)" in
        Linux) install_linux ;;
        Darwin)
            echo "tools: missing on this Mac: $gap" >&2
            echo "       brew install e2fsprogs" >&2
            echo "       (keg-only: the tests find it under \$(brew --prefix e2fsprogs)/sbin)" >&2
            exit 1
            ;;
        *) echo "tools: unsupported host $(uname -s)" >&2; exit 1 ;;
    esac
    gap="$(missing)"
fi

if [ -n "$gap" ]; then
    echo "tools: missing: $gap — run 'chore tools'" >&2
    exit 1
fi

version="$("$(find_tool mke2fs)" -V 2>&1 | sed -n 's/^mke2fs \([0-9][0-9.]*\).*/\1/p' | head -1)"
if [ -z "$version" ] || ! version_ge "$version" "$MIN_E2FSPROGS"; then
    echo "tools: e2fsprogs ${version:-of unknown version} is older than $MIN_E2FSPROGS" >&2
    exit 1
fi
for t in $TOOLS; do
    printf '  %-9s %s\n' "$t" "$(find_tool "$t")"
done
echo "tools: e2fsprogs $version — all oracle tools present"
