#!/bin/bash
# Build the ext4 test-disk fixtures inside a qemu-hosted Alpine Linux VM.
#
# Why qemu: mkfs.ext4, loop-mount, setfattr, setfacl — all Linux-only.
# qemu works everywhere (macOS, Linux, in CI), so one script drives
# the build on any host. Nothing about ext4rs itself touches platform
# specifics; this is just a build-time convenience.
#
# First run downloads Alpine's netboot kernel + initramfs + ISO
# (~40 MB total) into .vm-cache/. Subsequent runs reuse the cache.
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
# Requires: qemu-system-x86_64, ssh-keygen, bsdtar, curl.
# All available on macOS (brew install qemu) and ubuntu-latest.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

CACHE="$SCRIPT_DIR/.vm-cache"
mkdir -p "$CACHE"

# ---------------------------------------------------------------------------
# Parse flags.
# ---------------------------------------------------------------------------
SERVER_MODE=0
BATCH_ARGS=()
for arg in "$@"; do
    case "$arg" in
        --server) SERVER_MODE=1 ;;
        *) BATCH_ARGS+=("$arg") ;;
    esac
done

# ---------------------------------------------------------------------------
# Step 1 — pin Alpine version + download netboot assets on first run.
# ---------------------------------------------------------------------------
ALPINE_VER=3.21.4
ALPINE_REL="${ALPINE_VER%.*}"
ALPINE_ISO="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_REL}/releases/x86_64/alpine-virt-${ALPINE_VER}-x86_64.iso"
ALPINE_MAIN="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_REL}/main/x86_64"

# Pinned package versions for attr + acl (not in the virt ISO's
# embedded apk cache; downloaded separately and extracted in-place).
ATTR_APK="attr-2.5.2-r2.apk"
LIBATTR_APK="libattr-2.5.2-r2.apk"
ACL_APK="acl-2.3.2-r1.apk"
ACL_LIBS_APK="acl-libs-2.3.2-r1.apk"

download_if_missing() {
    local url="$1" out="$2"
    if [ ! -s "$out" ]; then
        echo "[host] downloading $(basename "$out")..."
        curl -fsSL -o "$out" "$url"
    fi
}
download_if_missing "$ALPINE_ISO" "$CACHE/alpine-virt.iso"

# Extract the ISO's kernel + initramfs.
if [ ! -s "$CACHE/vmlinuz-virt" ] || [ ! -s "$CACHE/initramfs-virt" ]; then
    echo "[host] extracting kernel + initramfs from alpine-virt ISO..."
    bsdtar -xf "$CACHE/alpine-virt.iso" -C "$CACHE" \
        boot/vmlinuz-virt boot/initramfs-virt
    cp "$CACHE/boot/vmlinuz-virt"   "$CACHE/vmlinuz-virt"
    cp "$CACHE/boot/initramfs-virt" "$CACHE/initramfs-virt"
fi

mkdir -p "$CACHE/extra-apks"
download_if_missing "$ALPINE_MAIN/$ATTR_APK"     "$CACHE/extra-apks/$ATTR_APK"
download_if_missing "$ALPINE_MAIN/$LIBATTR_APK"  "$CACHE/extra-apks/$LIBATTR_APK"
download_if_missing "$ALPINE_MAIN/$ACL_APK"      "$CACHE/extra-apks/$ACL_APK"
download_if_missing "$ALPINE_MAIN/$ACL_LIBS_APK" "$CACHE/extra-apks/$ACL_LIBS_APK"

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
OVL_TMP="$CACHE/ovl"
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
    # Server mode: mount 9p, install attr/acl + openssh (from CDN via
    # qemu user networking), set up sshd for root login, stay running.
    cat > "$OVL_TMP/etc/local.d/99-ext4.start" <<'WRAPPER_EOF'
#!/bin/sh
exec > /dev/console 2>&1
echo "=== [vm] server mode starting ==="

modprobe 9p 9pnet 9pnet_virtio loop 2>/dev/null || true

mkdir -p /host
if ! mount -t 9p -o trans=virtio,version=9p2000.L,msize=131072 host /host; then
    echo "=== [vm] 9p mount failed — aborting ==="
    poweroff -f
fi
echo "=== [vm] /host mounted ==="

# attr + acl from 9p share (same as batch mode).
for pkg in /host/.vm-cache/extra-apks/*.apk; do
    tar -xzf "$pkg" -C / --exclude=.PKGINFO --exclude='.SIGN.*' \
        --exclude=.pre-install --exclude=.post-install \
        --exclude=.pre-upgrade --exclude=.post-upgrade 2>/dev/null || true
done

# Bring up network (qemu SLIRP provides DHCP + DNS at 10.0.2.3).
ip link set eth0 up 2>/dev/null || true
udhcpc -i eth0 -t 10 -T 1 -q 2>/dev/null || true
echo "nameserver 10.0.2.3" > /etc/resolv.conf

# Install openssh from Alpine CDN.
echo "https://dl-cdn.alpinelinux.org/alpine/v3.21/main" >> /etc/apk/repositories
apk add --no-cache openssh-server openssh-keygen util-linux 2>/dev/null

# Generate host keys + configure root access.
ssh-keygen -A >/dev/null 2>&1
mkdir -p /root/.ssh
chmod 700 /root/.ssh
cat /host/.vm-cache/builder-key.pub > /root/.ssh/authorized_keys
chmod 600 /root/.ssh/authorized_keys
echo "PermitRootLogin yes"            >> /etc/ssh/sshd_config
echo "PasswordAuthentication no"      >> /etc/ssh/sshd_config
echo "StrictModes no"                 >> /etc/ssh/sshd_config

/usr/sbin/sshd
echo "=== [vm] sshd ready ==="
touch /host/.vm-cache/server-ready
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

for pkg in /host/.vm-cache/extra-apks/*.apk; do
    echo "=== [vm] extracting $(basename "$pkg") ==="
    tar -xzf "$pkg" -C / --exclude=.PKGINFO --exclude='.SIGN.*' \
        --exclude=.pre-install --exclude=.post-install \
        --exclude=.pre-upgrade --exclude=.post-upgrade 2>/dev/null || true
done

echo "=== [vm] running _vm-builder.sh ==="
if sh /host/_vm-builder.sh /host $(cat /host/.vm-cache/vm-args 2>/dev/null) \
        > /host/.vm-cache/vm-build.log 2>&1; then
    touch /host/.vm-cache/vm-build.done
    echo "=== [vm] builder succeeded ==="
else
    touch /host/.vm-cache/vm-build.failed
    echo "=== [vm] builder FAILED ==="
    tail -n 20 /host/.vm-cache/vm-build.log
fi

sync
poweroff -f
WRAPPER_EOF
fi

chmod +x "$OVL_TMP/etc/local.d/99-ext4.start"
ln -sf /etc/init.d/local "$OVL_TMP/etc/runlevels/default/local"

OVL_STAGE="$CACHE/ovl-iso-stage"
rm -rf "$OVL_STAGE" "$CACHE/ovl.iso"
mkdir -p "$OVL_STAGE"
(cd "$OVL_TMP" && tar -czf "$OVL_STAGE/localhost.apkovl.tar.gz" etc)
bsdtar -c -f "$CACHE/ovl.iso" --format=iso9660 -C "$OVL_STAGE" .

# ---------------------------------------------------------------------------
# Step 4 — boot Alpine under qemu.
# ---------------------------------------------------------------------------
if [[ "$SERVER_MODE" == "1" ]]; then
    # Pick a free port.
    EXT4_BUILDER_PORT="${EXT4_BUILDER_PORT:-2222}"
    rm -f "$CACHE/server-ready" "$CACHE/server.env"

    echo "[host] starting Alpine builder VM (SSH on localhost:${EXT4_BUILDER_PORT})..."
    qemu-system-x86_64 \
        -kernel "$CACHE/vmlinuz-virt" \
        -initrd "$CACHE/initramfs-virt" \
        -append "console=ttyS0 modules=loop,squashfs,sd-mod,usb-storage,virtio_blk,virtio_net,virtio_pci,9p,9pnet_virtio" \
        -drive file="$CACHE/alpine-virt.iso",media=cdrom,readonly=on,if=ide,index=0 \
        -drive file="$CACHE/ovl.iso",media=cdrom,readonly=on,if=ide,index=1 \
        -virtfs local,path="$SCRIPT_DIR",mount_tag=host,security_model=mapped-xattr,id=host \
        -netdev "user,id=net0,hostfwd=tcp::${EXT4_BUILDER_PORT}-:22" \
        -device virtio-net-pci,netdev=net0 \
        -m 1024 \
        -smp 2 \
        -nographic \
        -no-reboot \
        > "$CACHE/server-boot.log" 2>&1 &

    QEMU_PID=$!

    # Wait for sshd to signal readiness via the 9p share marker.
    echo "[host] waiting for sshd (up to 120s)..."
    timeout=120
    while [[ $timeout -gt 0 ]]; do
        if [[ -f "$CACHE/server-ready" ]]; then
            break
        fi
        if ! kill -0 "$QEMU_PID" 2>/dev/null; then
            echo "[host] qemu exited unexpectedly. Boot log:" >&2
            tail -n 40 "$CACHE/server-boot.log" >&2 || true
            exit 1
        fi
        sleep 1
        timeout=$((timeout - 1))
    done

    if [[ ! -f "$CACHE/server-ready" ]]; then
        echo "[host] timed out waiting for sshd. Boot log:" >&2
        tail -n 40 "$CACHE/server-boot.log" >&2 || true
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
    rm -f "$CACHE/vm-build.done" "$CACHE/vm-build.failed" "$CACHE/vm-build.log"
    printf '%s\n' "${BATCH_ARGS[@]+"${BATCH_ARGS[@]}"}" > "$CACHE/vm-args"

    echo "[host] booting Alpine under qemu (serial -> stdout)..."
    qemu-system-x86_64 \
        -kernel "$CACHE/vmlinuz-virt" \
        -initrd "$CACHE/initramfs-virt" \
        -append "console=ttyS0 modules=loop,squashfs,sd-mod,usb-storage,virtio_blk,virtio_net,virtio_pci,9p,9pnet_virtio" \
        -drive file="$CACHE/alpine-virt.iso",media=cdrom,readonly=on,if=ide,index=0 \
        -drive file="$CACHE/ovl.iso",media=cdrom,readonly=on,if=ide,index=1 \
        -virtfs local,path="$SCRIPT_DIR",mount_tag=host,security_model=mapped-xattr,id=host \
        -m 1024 \
        -smp 2 \
        -nographic \
        -no-reboot

    # ---------------------------------------------------------------------------
    # Step 5 — inspect the done-marker the guest left behind.
    # ---------------------------------------------------------------------------
    if [ -f "$CACHE/vm-build.done" ]; then
        echo "[host] guest reported success."
        exit 0
    elif [ -f "$CACHE/vm-build.failed" ]; then
        echo "[host] guest reported failure. Last 50 lines of vm-build.log:" >&2
        tail -n 50 "$CACHE/vm-build.log" >&2 || true
        exit 1
    else
        echo "[host] guest exited without writing a done marker — something" >&2
        echo "       went wrong during boot. Check earlier serial output." >&2
        exit 1
    fi
fi
