#!/bin/bash
# Build the ext4 test-disk fixtures inside a qemu-hosted Alpine Linux VM.
#
# Why qemu: mkfs.ext4, loop-mount, setfattr, setfacl — all Linux-only.
# qemu works everywhere (macOS, Linux, in CI), so one script drives
# the build on any host. Nothing about ext4rs itself touches platform
# specifics; this is just a build-time convenience.
#
# First run downloads Alpine's netboot kernel + initramfs + ISO
# (~40 MB total) into .vm-cache/. Subsequent runs reuse the cache. The guest
# follows the host architecture by default; set EXT4_VM_ARCH=x86_64 or
# EXT4_VM_ARCH=aarch64 to exercise the other supported path deliberately.
#
# Usage:
#   bash build-ext4-feature-images.sh              # build all images
#   bash build-ext4-feature-images.sh htree xattr  # build named ones
#
#   bash build-ext4-feature-images.sh --server     # start persistent VM
#     Boots Alpine with SSH and returns immediately. Writes connection
#     details to .vm-cache/server.env. The caller can then SSH in and
#     invoke _vm-builder.sh on demand (per-scenario image builds).
#     Stop the VM by sourcing server.env and killing EXT4_BUILDER_PID,
#     or by SSH-ing in and running `poweroff`.
#
# Requires: the host-matching qemu-system binary, ssh-keygen, bsdtar, curl.
# All available on macOS (brew install qemu) and ubuntu-latest.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# shellcheck source=vm-architecture.sh
. "$SCRIPT_DIR/vm-architecture.sh"

CACHE="$SCRIPT_DIR/.vm-cache"
mkdir -p "$CACHE"

# ---------------------------------------------------------------------------
# Parse flags.
# ---------------------------------------------------------------------------
SERVER_MODE=0
PRINT_VM_CONFIG=0
BATCH_ARGS=()
for arg in "$@"; do
    case "$arg" in
        --server) SERVER_MODE=1 ;;
        --print-vm-config) PRINT_VM_CONFIG=1 ;;
        *) BATCH_ARGS+=("$arg") ;;
    esac
done

if [[ "$PRINT_VM_CONFIG" == "1" ]]; then
    printf 'EXT4_VM_ARCH=%s\n' "$EXT4_VM_ARCH"
    printf 'ALPINE_ARCH=%s\n' "$ALPINE_ARCH"
    printf 'QEMU_SYSTEM=%s\n' "$QEMU_SYSTEM"
    printf 'QEMU_DRIVE_IF=%s\n' "$QEMU_DRIVE_IF"
    printf 'QEMU_CONSOLE=%s\n' "$QEMU_CONSOLE"
    printf 'QEMU_ACCEL=%s\n' "$QEMU_ACCEL"
    printf 'QEMU_BOOT_MODE=%s\n' "$QEMU_BOOT_MODE"
    printf 'QEMU_FIRMWARE=%s\n' "$QEMU_FIRMWARE"
    exit 0
fi

if ! command -v "$QEMU_SYSTEM" >/dev/null 2>&1; then
    echo "missing fixture VM emulator: $QEMU_SYSTEM" >&2
    exit 1
fi
if [[ "$QEMU_BOOT_MODE" == "uefi" && ! -f "$QEMU_FIRMWARE" ]]; then
    echo "missing AArch64 UEFI firmware for the fixture VM" >&2
    echo "install qemu-efi-aarch64, or set EXT4_QEMU_EFI to its firmware file" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Step 1 — pin Alpine version + download netboot assets on first run.
# ---------------------------------------------------------------------------
ALPINE_VER=3.21.4
ALPINE_REL="${ALPINE_VER%.*}"
ALPINE_ISO="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_REL}/releases/${ALPINE_ARCH}/alpine-virt-${ALPINE_VER}-${ALPINE_ARCH}.iso"
ALPINE_MAIN="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_REL}/main/${ALPINE_ARCH}"

# Pinned package versions for extra tools not in the virt ISO's
# embedded apk cache; downloaded separately and extracted in-place.
ATTR_APK="attr-2.5.2-r2.apk"
LIBATTR_APK="libattr-2.5.2-r2.apk"
ACL_APK="acl-2.3.2-r1.apk"
ACL_LIBS_APK="acl-libs-2.3.2-r1.apk"
# sfdisk + losetup (util-linux splits) for whole-disk GPT image builds.
# libfdisk/libsmartcols/libncursesw are their shared-library deps.
# libblkid and libuuid are already present (pulled in by e2fsprogs).
SFDISK_APK="sfdisk-2.40.4-r1.apk"
LOSETUP_APK="losetup-2.40.4-r1.apk"
LIBFDISK_APK="libfdisk-2.40.4-r1.apk"
LIBSMARTCOLS_APK="libsmartcols-2.40.4-r1.apk"
LIBNCURSESW_APK="libncursesw-6.5_p20241006-r3.apk"
NCURSES_TERMINFO_APK="ncurses-terminfo-base-6.5_p20241006-r3.apk"
# openssh for server mode sshd — pre-downloaded so no network needed at boot.
# libcrypto3 and zlib are already present (alpine-base dependency chain).
OPENSSH_SERVER_APK="openssh-server-9.9_p2-r0.apk"
OPENSSH_SERVER_COMMON_APK="openssh-server-common-9.9_p2-r0.apk"
OPENSSH_KEYGEN_APK="openssh-keygen-9.9_p2-r0.apk"

download_if_missing() {
    local url="$1" out="$2"
    if [ ! -s "$out" ]; then
        echo "[host] downloading $(basename "$out")..."
        curl -fsSL -o "$out" "$url"
    fi
}
ALPINE_ISO_PATH="$CACHE/alpine-virt-${ALPINE_ARCH}.iso"
VMLINUX_PATH="$CACHE/vmlinuz-virt-${ALPINE_ARCH}"
INITRAMFS_PATH="$CACHE/initramfs-virt-${ALPINE_ARCH}"
EXTRA_APKS="$CACHE/extra-apks/${ALPINE_ARCH}"

download_if_missing "$ALPINE_ISO" "$ALPINE_ISO_PATH"

# Extract the ISO's kernel + initramfs.
if [ ! -s "$VMLINUX_PATH" ] || [ ! -s "$INITRAMFS_PATH" ]; then
    echo "[host] extracting kernel + initramfs from alpine-virt ISO..."
    bsdtar -xf "$ALPINE_ISO_PATH" -C "$CACHE" \
        boot/vmlinuz-virt boot/initramfs-virt
    cp "$CACHE/boot/vmlinuz-virt"   "$VMLINUX_PATH"
    cp "$CACHE/boot/initramfs-virt" "$INITRAMFS_PATH"
fi

mkdir -p "$EXTRA_APKS"
download_if_missing "$ALPINE_MAIN/$ATTR_APK"          "$EXTRA_APKS/$ATTR_APK"
download_if_missing "$ALPINE_MAIN/$LIBATTR_APK"       "$EXTRA_APKS/$LIBATTR_APK"
download_if_missing "$ALPINE_MAIN/$ACL_APK"           "$EXTRA_APKS/$ACL_APK"
download_if_missing "$ALPINE_MAIN/$ACL_LIBS_APK"      "$EXTRA_APKS/$ACL_LIBS_APK"
download_if_missing "$ALPINE_MAIN/$SFDISK_APK"        "$EXTRA_APKS/$SFDISK_APK"
download_if_missing "$ALPINE_MAIN/$LOSETUP_APK"       "$EXTRA_APKS/$LOSETUP_APK"
download_if_missing "$ALPINE_MAIN/$LIBFDISK_APK"      "$EXTRA_APKS/$LIBFDISK_APK"
download_if_missing "$ALPINE_MAIN/$LIBSMARTCOLS_APK"  "$EXTRA_APKS/$LIBSMARTCOLS_APK"
download_if_missing "$ALPINE_MAIN/$LIBNCURSESW_APK"   "$EXTRA_APKS/$LIBNCURSESW_APK"
download_if_missing "$ALPINE_MAIN/$NCURSES_TERMINFO_APK" "$EXTRA_APKS/$NCURSES_TERMINFO_APK"
download_if_missing "$ALPINE_MAIN/$OPENSSH_SERVER_APK"        "$EXTRA_APKS/$OPENSSH_SERVER_APK"
download_if_missing "$ALPINE_MAIN/$OPENSSH_SERVER_COMMON_APK" "$EXTRA_APKS/$OPENSSH_SERVER_COMMON_APK"
download_if_missing "$ALPINE_MAIN/$OPENSSH_KEYGEN_APK"        "$EXTRA_APKS/$OPENSSH_KEYGEN_APK"
# ---------------------------------------------------------------------------
# Step 2 (server mode only) — generate SSH keypair for builder VM access.
# ---------------------------------------------------------------------------
if [[ "$SERVER_MODE" == "1" ]]; then
    if [[ ! -f "$CACHE/builder-key" ]]; then
        echo "[host] generating builder VM SSH keypair..."
        ssh-keygen -t ed25519 -f "$CACHE/builder-key" -N "" -C "ext4-builder-vm" >/dev/null
    fi
fi

# ---------------------------------------------------------------------------
# Step 3 — assemble the apkovl (Alpine overlay).
# ---------------------------------------------------------------------------
OVL_TMP="$CACHE/ovl-$ALPINE_ARCH"
rm -rf "$OVL_TMP"
mkdir -p \
    "$OVL_TMP/etc/local.d" \
    "$OVL_TMP/etc/runlevels/sysinit" \
    "$OVL_TMP/etc/runlevels/boot" \
    "$OVL_TMP/etc/runlevels/default" \
    "$OVL_TMP/etc/apk"

for svc in devfs dmesg mdev hwdrivers modloop; do
    ln -sf /etc/init.d/"$svc" "$OVL_TMP/etc/runlevels/sysinit/$svc"
done
for svc in bootmisc hostname hwclock modules sysctl syslog urandom; do
    ln -sf /etc/init.d/"$svc" "$OVL_TMP/etc/runlevels/boot/$svc"
done

cat > "$OVL_TMP/etc/apk/world" <<'PKGS_EOF'
alpine-base
busybox
e2fsprogs
e2fsprogs-extra
PKGS_EOF

cat > "$OVL_TMP/etc/apk/repositories" <<'REPO_EOF'
/media/cdrom/apks
REPO_EOF

if [[ "$SERVER_MODE" == "1" ]]; then
    # Server mode: mount 9p, extract all extra-apks (includes openssh +
    # sfdisk/losetup), set up sshd for root login, stay running.
    # Two 9p shares:
    #   host  → SERVER_IMAGE_DIR (where finished images land)
    #   cache → CACHE (.vm-cache: APKs, builder-key, ready signal)
    cat > "$OVL_TMP/etc/local.d/99-ext4.start" <<'WRAPPER_EOF'
#!/bin/sh
exec > /dev/console 2>&1
echo "=== [vm] server mode starting ==="

modprobe 9p 9pnet 9pnet_virtio loop 2>/dev/null || true

mkdir -p /host /cache
if ! mount -t 9p -o trans=virtio,version=9p2000.L,msize=131072 host /host; then
    echo "=== [vm] 9p mount (host) failed — aborting ==="
    poweroff -f
fi
if ! mount -t 9p -o trans=virtio,version=9p2000.L,msize=131072 cache /cache; then
    echo "=== [vm] 9p mount (cache) failed — aborting ==="
    poweroff -f
fi
echo "=== [vm] /host mounted ==="

# Extract all extra-apks: attr, acl, sfdisk, losetup, openssh, etc.
for pkg in /cache/extra-apks/__ALPINE_ARCH__/*.apk; do
    tar -xzf "$pkg" -C / --exclude=.PKGINFO --exclude='.SIGN.*' \
        --exclude=.pre-install --exclude=.post-install \
        --exclude=.pre-upgrade --exclude=.post-upgrade 2>/dev/null || true
done

# Bring up network so sshd binds to the SLIRP interface (10.0.2.15).
ip link set eth0 up 2>/dev/null || true
udhcpc -i eth0 -t 10 -T 1 -q 2>/dev/null || true

# Generate host keys + configure root access.
ssh-keygen -A >/dev/null 2>&1
mkdir -p /root/.ssh
chmod 700 /root/.ssh
cat /cache/builder-key.pub > /root/.ssh/authorized_keys
chmod 600 /root/.ssh/authorized_keys
echo "PermitRootLogin yes"            >> /etc/ssh/sshd_config
echo "PasswordAuthentication no"      >> /etc/ssh/sshd_config
echo "StrictModes no"                 >> /etc/ssh/sshd_config

if [ ! -x /usr/sbin/sshd ]; then
    echo "=== [vm] sshd not found — openssh not installed ==="
    poweroff -f
fi
/usr/sbin/sshd
echo "=== [vm] sshd ready ==="
touch /cache/server-ready-__ALPINE_ARCH__
WRAPPER_EOF
else
    # Batch mode: run builder then power off (original behaviour).
    cat > "$OVL_TMP/etc/local.d/99-ext4.start" <<'WRAPPER_EOF'
#!/bin/sh
exec > /dev/console 2>&1
echo "=== [vm] local.d starting ==="

modprobe 9p 9pnet 9pnet_virtio loop 2>/dev/null || true

mkdir -p /host
if ! mount -t 9p -o trans=virtio,version=9p2000.L,msize=131072 host /host; then
    echo "=== [vm] 9p mount failed — aborting ==="
    poweroff -f
fi
echo "=== [vm] /host mounted ==="

for pkg in /host/.vm-cache/extra-apks/__ALPINE_ARCH__/*.apk; do
    echo "=== [vm] extracting $(basename "$pkg") ==="
    tar -xzf "$pkg" -C / --exclude=.PKGINFO --exclude='.SIGN.*' \
        --exclude=.pre-install --exclude=.post-install \
        --exclude=.pre-upgrade --exclude=.post-upgrade 2>/dev/null || true
done

echo "=== [vm] running _vm-builder.sh ==="
if sh /host/_vm-builder.sh /host $(cat /host/.vm-cache/vm-args-__ALPINE_ARCH__ 2>/dev/null) \
        > /host/.vm-cache/vm-build-__ALPINE_ARCH__.log 2>&1; then
    touch /host/.vm-cache/vm-build-__ALPINE_ARCH__.done
    echo "=== [vm] builder succeeded ==="
else
    touch /host/.vm-cache/vm-build-__ALPINE_ARCH__.failed
    echo "=== [vm] builder FAILED ==="
    tail -n 20 /host/.vm-cache/vm-build-__ALPINE_ARCH__.log
fi

sync
poweroff -f
WRAPPER_EOF
fi

sed -i "s/__ALPINE_ARCH__/$ALPINE_ARCH/g" "$OVL_TMP/etc/local.d/99-ext4.start"
chmod +x "$OVL_TMP/etc/local.d/99-ext4.start"
ln -sf /etc/init.d/local "$OVL_TMP/etc/runlevels/default/local"

OVL_STAGE="$CACHE/ovl-iso-stage-$ALPINE_ARCH"
OVL_ISO="$CACHE/ovl-$ALPINE_ARCH.iso"
rm -rf "$OVL_STAGE" "$OVL_ISO"
mkdir -p "$OVL_STAGE"
(cd "$OVL_TMP" && tar -czf "$OVL_STAGE/localhost.apkovl.tar.gz" etc)
bsdtar -c -f "$OVL_ISO" --format=iso9660 -C "$OVL_STAGE" .

# ---------------------------------------------------------------------------
# Step 4 — boot Alpine under qemu.
# ---------------------------------------------------------------------------
if [[ "$QEMU_DRIVE_IF" == "ide" ]]; then
    QEMU_BOOT_DRIVES=(
        -drive "file=$ALPINE_ISO_PATH,media=cdrom,readonly=on,if=ide,index=0"
        -drive "file=$OVL_ISO,media=cdrom,readonly=on,if=ide,index=1"
    )
else
    QEMU_BOOT_DRIVES=(
        -drive "file=$ALPINE_ISO_PATH,format=raw,readonly=on,if=virtio,index=0"
        -drive "file=$OVL_ISO,format=raw,readonly=on,if=virtio,index=1"
    )
fi

if [[ "$QEMU_BOOT_MODE" == "direct" ]]; then
    QEMU_BOOT_ARGS=(
        -kernel "$VMLINUX_PATH"
        -initrd "$INITRAMFS_PATH"
        -append "console=$QEMU_CONSOLE modules=loop,squashfs,sd-mod,usb-storage,virtio_blk,virtio_net,virtio_pci,9p,9pnet_virtio"
    )
else
    QEMU_BOOT_ARGS=(-bios "$QEMU_FIRMWARE")
fi

if [[ "$SERVER_MODE" == "1" ]]; then
    # Pick a free port.
    EXT4_BUILDER_PORT="${EXT4_BUILDER_PORT:-2222}"
    SERVER_READY="$CACHE/server-ready-$ALPINE_ARCH"
    rm -f "$SERVER_READY" "$CACHE/server.env"

    # In server mode the 9p share must point at HOST_IMAGE_DIR (where the
    # harness expects to find finished images) rather than SCRIPT_DIR.
    # HOST_IMAGE_DIR is exported by run-matrix.sh after reading .test-env.
    # Default to SCRIPT_DIR only when running standalone (outside the harness).
    SERVER_IMAGE_DIR="${HOST_IMAGE_DIR:-$SCRIPT_DIR}"
    mkdir -p "$SERVER_IMAGE_DIR"

    # Make _vm-builder.sh accessible inside the VM via the 9p share.
    cp "$SCRIPT_DIR/_vm-builder.sh" "$SERVER_IMAGE_DIR/_vm-builder.sh"

    echo "[host] starting Alpine builder VM (SSH on localhost:${EXT4_BUILDER_PORT})..."
    "$QEMU_SYSTEM" \
        "${QEMU_MACHINE_ARGS[@]}" \
        "${QEMU_BOOT_ARGS[@]}" \
        "${QEMU_BOOT_DRIVES[@]}" \
        -virtfs local,path="$SERVER_IMAGE_DIR",mount_tag=host,security_model=mapped-xattr,id=host \
        -virtfs local,path="$CACHE",mount_tag=cache,security_model=mapped-xattr,id=cache \
        -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:${EXT4_BUILDER_PORT}-:22" \
        -device virtio-net-pci,netdev=net0 \
        -m 1024 \
        -smp 2 \
        -nographic \
        -no-reboot \
        &

    QEMU_PID=$!

    # Wait for sshd to signal readiness via the 9p share marker.
    echo "[host] waiting for sshd (up to 120s)..."
    timeout=120
    while [[ $timeout -gt 0 ]]; do
        if [[ -f "$SERVER_READY" ]]; then
            break
        fi
        if ! kill -0 "$QEMU_PID" 2>/dev/null; then
            echo "[host] qemu exited unexpectedly" >&2
            exit 1
        fi
        sleep 1
        timeout=$((timeout - 1))
    done

    if [[ ! -f "$SERVER_READY" ]]; then
        echo "[host] timed out waiting for sshd" >&2
        kill "$QEMU_PID" 2>/dev/null || true
        exit 1
    fi

    # Write connection details for the consumer (run-matrix.sh).
    cat > "$CACHE/server.env" <<EOF
EXT4_BUILDER_PORT=${EXT4_BUILDER_PORT}
EXT4_BUILDER_KEY=${CACHE}/builder-key
EXT4_BUILDER_PID=${QEMU_PID}
EOF
    echo "[host] builder VM ready — SSH on localhost:${EXT4_BUILDER_PORT} (pid ${QEMU_PID})"
    echo "[host] connection details written to ${CACHE}/server.env"

else
    # Batch mode — existing behaviour.
    VM_BUILD_DONE="$CACHE/vm-build-$ALPINE_ARCH.done"
    VM_BUILD_FAILED="$CACHE/vm-build-$ALPINE_ARCH.failed"
    VM_BUILD_LOG="$CACHE/vm-build-$ALPINE_ARCH.log"
    VM_ARGS="$CACHE/vm-args-$ALPINE_ARCH"
    rm -f "$VM_BUILD_DONE" "$VM_BUILD_FAILED" "$VM_BUILD_LOG"
    printf '%s\n' "${BATCH_ARGS[@]+"${BATCH_ARGS[@]}"}" > "$VM_ARGS"

    echo "[host] booting Alpine $ALPINE_ARCH under $QEMU_SYSTEM (serial -> stdout)..."
    "$QEMU_SYSTEM" \
        "${QEMU_MACHINE_ARGS[@]}" \
        "${QEMU_BOOT_ARGS[@]}" \
        "${QEMU_BOOT_DRIVES[@]}" \
        -virtfs local,path="$SCRIPT_DIR",mount_tag=host,security_model=mapped-xattr,id=host \
        -m 1024 \
        -smp 2 \
        -nographic \
        -no-reboot

    # ---------------------------------------------------------------------------
    # Step 5 — inspect the done-marker the guest left behind.
    # ---------------------------------------------------------------------------
    if [ -f "$VM_BUILD_DONE" ]; then
        echo "[host] guest reported success."
        exit 0
    elif [ -f "$VM_BUILD_FAILED" ]; then
        echo "[host] guest reported failure. Last 50 lines of $VM_BUILD_LOG:" >&2
        tail -n 50 "$VM_BUILD_LOG" >&2 || true
        exit 1
    else
        echo "[host] guest exited without writing a done marker — something" >&2
        echo "       went wrong during boot. Check earlier serial output." >&2
        exit 1
    fi
fi
