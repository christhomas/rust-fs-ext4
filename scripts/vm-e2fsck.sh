#!/usr/bin/env bash
#
# vm-e2fsck.sh <image> [<image>...] — run `e2fsck -fn` on each image
# inside the Debian arm64 oracle VM.
#
# This is the real-Linux-ext4 oracle for driver-mutated images: no host
# e2fsprogs, no Docker, and no marking our own homework. A driver that
# only checks its own work against its own reader proves consistency,
# not correctness.
#
# The interface is unchanged from the version that drove an emulated
# x86_64 Alpine guest — same arguments, same exit semantics — so callers
# port across untouched. What changed underneath is the guest: an arm64
# Debian VM under QEMU/HVF runs at hardware speed, where the x86_64
# guest meant full CPU emulation on Apple Silicon for every check.
#
# Exit status is non-zero if e2fsck reported a problem with any image.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SHARE="$REPO/.vm-share"
VM="$REPO/scripts/vm.sh"

[ $# -gt 0 ] || { echo "usage: vm-e2fsck.sh <image> [<image>...]" >&2; exit 2; }

"$VM" up
mkdir -p "$SHARE"

# Stage copies rather than the originals: e2fsck is run with -n so it
# does not write, but a fixture is not worth risking to prove that.
#
# EACH COPY IS NAMED BY THIS SCRIPT, NOT BY THE CALLER (#153). The name
# goes into a command string the guest re-parses as root, so a basename
# carrying `;` or `$(...)` ran there; and two inputs sharing a basename
# (`a/test.img`, `b/test.img`) were staged over each other, so the second
# check read the first image and passed a file it never looked at. A
# name made of this process's id and the argument's position is unique
# per input and holds nothing a shell would act on. The report still
# names the path the caller passed.
for img in "$@"; do
    [ -f "$img" ] || { echo "no such image: $img" >&2; exit 2; }
done
staged=()
for i in $(seq 1 $#); do
    staged+=("vm-e2fsck-$$-$i.img")
done
trap 'for name in "${staged[@]}"; do rm -f "$SHARE/$name"; done' EXIT
i=0
for img in "$@"; do
    cp "$img" "$SHARE/${staged[$i]}"
    i=$((i + 1))
done

rc=0
i=0
for img in "$@"; do
    base="${staged[$i]}"
    i=$((i + 1))
    echo "############ e2fsck $img ############"
    # `-f` forces a full check even when the superblock says clean, and
    # `-n` answers no to every repair prompt, so this reports without
    # touching the image.
    if ! "$VM" run "e2fsck -fn /share/$base"; then
        rc=1
    fi
    echo
done

if [ "$rc" -ne 0 ]; then
    echo "e2fsck reported problems — see the output above." >&2
fi
exit "$rc"
