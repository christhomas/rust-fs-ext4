#!/usr/bin/env bash
#
# Cross-validate fs-ext4 images against FreeBSD's native ext2/3/4 driver,
# without Vagrant. Drives qemu-system-aarch64 directly:
#
#   1. Download FreeBSD's BASIC-CI cloud image (cloud-init enabled,
#      arm64 native — no CPU emulation on Apple Silicon hosts).
#   2. Build a NoCloud seed ISO from `user-data` + `meta-data`.
#   3. Boot qemu with the cloud image + seed ISO + every test-disks/*.img
#      attached as virtio-blk. cloud-init mounts each via mount_ext2fs,
#      hashes its files, dumps a manifest to the serial console, and
#      power-offs. The host captures the serial stream and parses it.
#
# Why not Vagrant: no FreeBSD box on Vagrant Cloud ships a `qemu`-format
# build. Vagrant's libvirt-via-qemu translation doesn't cover this case
# either. Direct qemu eliminates the box-format compatibility lottery.
#
# Why arm64-native: Apple Silicon hosts run aarch64 FreeBSD under HVF
# acceleration with zero CPU emulation overhead. x86_64 FreeBSD on the
# same host would need TCG (~50x slowdown). Pick the native arch first;
# fall back to x86_64 if the test contract specifically needs it.
#
# Spec sources:
#   - FreeBSD VM-IMAGES distribution layout: download.freebsd.org docs.
#   - cloud-init NoCloud datasource: cloudinit.readthedocs.io/en/latest/.
#   - virtio-blk device naming on FreeBSD: /dev/vd[b-z] for non-boot disks.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"

# All scratch state lives under .qemu/ alongside the .lwext4/ tree from
# the lwext4 cross-validator. Both are .gitignore'd.
WORK_DIR="${WORK_DIR:-$CRATE_DIR/.qemu}"
mkdir -p "$WORK_DIR"

FREEBSD_VERSION="${FREEBSD_VERSION:-14.3-RELEASE}"
FREEBSD_ARCH="${FREEBSD_ARCH:-arm64-aarch64}"
FREEBSD_VARIANT="BASIC-CLOUDINIT-ufs"  # cloud-init enabled, UFS root
IMG_NAME="FreeBSD-${FREEBSD_VERSION}-${FREEBSD_ARCH}-${FREEBSD_VARIANT}.qcow2"
IMG_URL="https://download.freebsd.org/releases/VM-IMAGES/${FREEBSD_VERSION}/${FREEBSD_ARCH##*-}/Latest/${IMG_NAME}.xz"
IMG_PATH="$WORK_DIR/$IMG_NAME"

SEED_DIR="$WORK_DIR/seed"
SEED_ISO="$WORK_DIR/seed.iso"

EDK2_FW="${EDK2_FW:-/opt/homebrew/share/qemu/edk2-aarch64-code.fd}"

# ---------------------------------------------------------------------------
# Step 1: download the FreeBSD cloud image (cached, ~500 MB compressed).
# ---------------------------------------------------------------------------
if [[ ! -f "$IMG_PATH" ]]; then
    echo "[qemu-fbsd] downloading $IMG_NAME"
    curl -fL -o "${IMG_PATH}.xz" "$IMG_URL"
    echo "[qemu-fbsd] decompressing"
    xz -d "${IMG_PATH}.xz"
fi

# ---------------------------------------------------------------------------
# Step 2: build the cloud-init NoCloud seed ISO.
# ---------------------------------------------------------------------------
echo "[qemu-fbsd] building cloud-init seed ISO"
mkdir -p "$SEED_DIR"
cp "$SCRIPT_DIR/user-data" "$SEED_DIR/user-data"
cp "$SCRIPT_DIR/meta-data" "$SEED_DIR/meta-data"
xorriso -as mkisofs \
    -volid CIDATA \
    -joliet -rock \
    -output "$SEED_ISO" \
    "$SEED_DIR" 2>/dev/null

# ---------------------------------------------------------------------------
# Step 3: collect test-disks/*.img into virtio-blk -drive args.
# ---------------------------------------------------------------------------
TEST_IMG_DIR="${TEST_IMG_DIR:-$CRATE_DIR/test-disks}"
DRIVE_ARGS=()
LETTER_IDX=1  # vda is the boot disk; secondary disks start at vdb
for img in "$TEST_IMG_DIR"/*.img; do
    [ -f "$img" ] || continue
    DRIVE_ARGS+=(-drive "file=${img},if=none,format=raw,id=img${LETTER_IDX}")
    DRIVE_ARGS+=(-device "virtio-blk-pci,drive=img${LETTER_IDX}")
    LETTER_IDX=$((LETTER_IDX + 1))
done
echo "[qemu-fbsd] attaching ${#DRIVE_ARGS[@]} virtio-blk args ($(((${#DRIVE_ARGS[@]} / 2))) test images)"

# ---------------------------------------------------------------------------
# Step 4: boot qemu, capture serial output, wait for cloud-init shutdown.
# ---------------------------------------------------------------------------
SERIAL_LOG="$WORK_DIR/serial.log"
: > "$SERIAL_LOG"
echo "[qemu-fbsd] booting (serial log: $SERIAL_LOG)"

# `-machine virt` is the standard aarch64 paravirt machine; `-cpu host`
# uses the underlying hardware CPU (HVF accel needed for that). `accel=hvf`
# is Apple's hypervisor framework — zero CPU emulation overhead.
# `-display none` keeps the run headless; serial→stdout captures
# everything we need.
qemu-system-aarch64 \
    -machine virt,accel=hvf,highmem=on \
    -cpu host \
    -smp 2 \
    -m 1024 \
    -drive if=pflash,format=raw,readonly=on,file="$EDK2_FW" \
    -drive file="$IMG_PATH",if=virtio,format=qcow2 \
    -drive file="$SEED_ISO",if=virtio,format=raw,readonly=on \
    "${DRIVE_ARGS[@]}" \
    -nographic \
    -serial file:"$SERIAL_LOG" \
    -monitor none \
    -no-reboot

# qemu exits when the VM powers off (cloud-init's `poweroff` at the end
# of user-data triggers this).

# ---------------------------------------------------------------------------
# Step 5: parse the serial log, extract per-image manifests.
# ---------------------------------------------------------------------------
if ! grep -q '\[manifest:end\]' "$SERIAL_LOG"; then
    echo "[qemu-fbsd] FAIL: cloud-init didn't reach manifest:end. Tail of serial log:"
    tail -40 "$SERIAL_LOG"
    exit 1
fi

MANIFEST_DIR="$WORK_DIR/manifests"
mkdir -p "$MANIFEST_DIR"
echo "[qemu-fbsd] manifests:"
awk '
    /^\[manifest:disk:.*:begin\]$/ {
        match($0, /disk:[^:]+/)
        name = substr($0, RSTART+5, RLENGTH-5)
        out = "'"$MANIFEST_DIR"'/" name ".manifest"
        in_manifest = 1
        next
    }
    /^\[manifest:disk:.*:end:/ {
        in_manifest = 0
        match($0, /:end:[^]]+/)
        status = substr($0, RSTART+5, RLENGTH-5)
        printf "  %s -> %s (%s)\n", name, out, status
        next
    }
    in_manifest { print > out }
' "$SERIAL_LOG"

echo "[qemu-fbsd] DONE. Per-image manifests in $MANIFEST_DIR/"
echo "[qemu-fbsd] Diff harness (compare against fs-ext4 mount) is future work —"
echo "[qemu-fbsd] same incremental approach as the lwext4 stub."
