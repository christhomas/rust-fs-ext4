#!/usr/bin/env bash
# vm-e2fsck.sh <image> [<image>...] — boot the cached Alpine builder VM and run
# `e2fsck -fn` on each image via the /host 9p share, then tear the VM down.
# This is the real-Linux-ext4 oracle for driver-mutated images, with no host
# e2fsprogs and no Docker. See AGENTS.md ("Cross-validation") for why and how.
set -euo pipefail
EXT4_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/vm-e2fsck.XXXXXX")"
for img in "$@"; do cp "$img" "$STAGE/$(basename "$img")"; done

cd "$EXT4_REPO"
export HOST_IMAGE_DIR="$STAGE"
# IMPORTANT: do NOT pipe the boot script (qemu inherits its stdout, so a pipe
# reader like `tail` never sees EOF and hangs). Redirect to a file instead.
bash test-disks/build-ext4-feature-images.sh --server > "$STAGE/boot.log" 2>&1
source test-disks/.vm-cache/server.env

# IMPORTANT: call ssh directly (zsh does not word-split an unquoted "$SSH"
# string) with an args array, a hard `timeout`, and keepalives (ConnectTimeout
# only bounds the TCP connect, not a post-connect stall).
OPTS=(-i "$EXT4_BUILDER_KEY" -p "$EXT4_BUILDER_PORT"
      -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
      -o ConnectTimeout=10 -o BatchMode=yes
      -o ServerAliveInterval=5 -o ServerAliveCountMax=2 root@localhost)

rc=0
for img in "$@"; do
  b="$(basename "$img")"
  echo "############ e2fsck $b ############"
  timeout 60 ssh "${OPTS[@]}" "e2fsck -fn /host/$b; echo e2fsck_EXIT=\$?" </dev/null || rc=1
  echo
done

kill "$EXT4_BUILDER_PID" 2>/dev/null || true
rm -f test-disks/.vm-cache/server.env test-disks/.vm-cache/server-ready
rm -rf "$STAGE"
exit $rc
