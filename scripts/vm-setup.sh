#!/usr/bin/env bash
#
# vm-setup.sh — the fs-linux-test-harness [setup] script. Runs as root
# INSIDE the VM, re-run by the harness whenever this file changes.
#
# Installs what test-disks/guest-build-images.sh needs to build the
# kernel-made fixtures, and nothing else: no Rust toolchain, and none of
# the host-side oracle tools' jobs (those run on the host, installed by
# `chore tools`).
#
#   e2fsprogs  mkfs.ext4, tune2fs
#   attr, acl  setfattr, setfacl
#   fdisk      sfdisk (GPT for the whole-disk image)
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq e2fsprogs attr acl fdisk >/dev/null
modprobe loop
# sed, not head: head exits after one line, mke2fs gets SIGPIPE writing
# its second, and pipefail turns that into a failed setup (seen on CI).
mkfs.ext4 -V 2>&1 | sed -n 1p
