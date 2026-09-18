#!/usr/bin/env bash
#
# vm-setup.sh — the fs-linux-test-harness [setup] script. Runs as root
# INSIDE the VM, re-applied by the harness whenever this file changes.
#
# THE GUEST IS WHERE THE ORACLE TOOLS LIVE. Not the host: e2fsprogs on a
# workstation is whatever that machine has — a keg-only Homebrew formula
# on a Mac, a distribution build on Linux, a different version per
# developer — and on a Mac it is not even the platform these images are
# for. One Debian guest, one version, the same answers for everyone.
#
#   e2fsprogs   mke2fs, mkfs.ext4, e2fsck, fsck.ext4, debugfs, dumpe2fs,
#               tune2fs — the oracle tools (tests/support/src/oracle.rs)
#               and the fixture builder's formatter
#   attr, acl   setfattr/getfattr, setfacl/getfacl: what the kernel
#               oracle reads back (tests/support/src/kernel.rs)
#   fdisk       sfdisk, for the whole-disk fixture's GPT
#   util-linux  losetup and mount: the kernel oracle's loop mounts, which
#               happen here and nowhere else
#
# AND A RUST TOOLCHAIN, for `chore test:vm` — the whole suite compiled
# and run in here, which is how a macOS host runs a Linux test suite at
# all. It is pinned to the repository's rust-toolchain.toml, installed
# under /var/lib (the VM's own disk, which outlives a `vm:down`), and the
# build directory lives there too so the second run is incremental.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

REPO=/repo
RUST_ROOT=/var/lib/fs-ext4-rust
export RUSTUP_HOME="$RUST_ROOT/rustup"
export CARGO_HOME="$RUST_ROOT/cargo"

apt-get update -qq
apt-get install -y -qq e2fsprogs attr acl fdisk util-linux curl gcc libc6-dev pkg-config >/dev/null
modprobe loop

# sed, not head: head exits after one line, mke2fs gets SIGPIPE writing
# its second, and pipefail turns that into a failed setup (seen on CI).
mkfs.ext4 -V 2>&1 | sed -n 1p

# 1.47.0 is the first e2fsprogs that knows `orphan_file`, which the
# oracle tests turn off by name (`-O ^orphan_file`); older releases
# refuse the option outright. Debian 12 ships exactly 1.47.0.
version="$(mke2fs -V 2>&1 | sed -n 's/^mke2fs \([0-9][0-9.]*\).*/\1/p' | head -1)"
if [ -z "$version" ] ||
    [ "$(printf '%s\n%s\n' 1.47.0 "$version" | sort -V | head -1)" != 1.47.0 ]; then
    echo "vm-setup: e2fsprogs ${version:-of unknown version} is older than 1.47.0" >&2
    exit 1
fi

# The toolchain the repository pins, and only that one: a guest that
# silently built with a different compiler than CI is a guest whose
# result means nothing.
toolchain="$(sed -n 's/^channel = "\([^"]*\)"/\1/p' "$REPO/rust-toolchain.toml" | head -1)"
[ -n "$toolchain" ] || { echo "vm-setup: no channel in $REPO/rust-toolchain.toml" >&2; exit 1; }

mkdir -p "$RUST_ROOT"
if [ ! -x "$CARGO_HOME/bin/rustup" ]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
        sh -s -- -y --no-modify-path --default-toolchain none >/dev/null
fi
"$CARGO_HOME/bin/rustup" toolchain install "$toolchain" \
    --component rustfmt --component clippy --profile minimal >/dev/null
"$CARGO_HOME/bin/rustup" default "$toolchain" >/dev/null
"$CARGO_HOME/bin/cargo" --version

echo "vm-setup: oracle tools and the pinned toolchain are installed in the guest"
