#!/bin/bash
# Build the ext4 test-disk fixtures inside a qemu-hosted Alpine Linux VM.
#
# Why qemu: mkfs.ext4, loop-mount, setfattr, setfacl — all Linux-only.
# qemu works everywhere (macOS, Linux, in CI), so one script drives
# the build on any host. Nothing about ext4rs itself touches platform
# specifics; this is just a build-time convenience.
#
# First run downloads Alpine's netboot kernel + initramfs + modloop
# (~40 MB total) into .vm-cache/. Subsequent runs reuse the cache.
#
# Usage:
#   bash build-ext4-feature-images.sh              # build all images
#   bash build-ext4-feature-images.sh htree xattr  # build named ones
#
# Requires: qemu-system-x86_64, python3 (for the tiny apkovl HTTP
# server), tar, curl. All available on macOS (brew install qemu),
# ubuntu-latest (apt install qemu-system-x86), and alpine/fedora.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

CACHE="$SCRIPT_DIR/.vm-cache"
mkdir -p "$CACHE"

# ---------------------------------------------------------------------------
# Step 1 — pin Alpine version + download netboot assets on first run.
# ---------------------------------------------------------------------------
ALPINE_VER=3.21.4
ALPINE_REL="${ALPINE_VER%.*}"
ALPINE_NETBOOT="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_REL}/releases/x86_64/netboot-${ALPINE_VER}"
ALPINE_ISO="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_REL}/releases/x86_64/alpine-virt-${ALPINE_VER}-x86_64.iso"
ALPINE_MAIN="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_REL}/main/x86_64"

# Pinned package versions for attr + acl (not in the virt ISO's
# embedded apk cache; downloaded separately and installed via
# `apk add --allow-untrusted` from the 9p share after boot).
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

# Extract the ISO's kernel + initramfs. The netboot ones differ —
# the ISO's initramfs is configured to expect a local apk cache on
# the boot CDROM and mounts it at /media/cdrom. The netboot one
# expects to fetch packages from alpine_repo= at install time,
# which doesn't suit our offline approach.
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
# Step 2 — assemble the apkovl (Alpine overlay) that wires our guest
# builder in as an auto-started local.d service.
# ---------------------------------------------------------------------------
OVL_TMP="$CACHE/ovl"
rm -rf "$OVL_TMP"
# sysinit runlevel: load modules + modloop so 9p-virtio gets
# initialised before local.d runs. boot runlevel: minimal basics.
# default runlevel: our ext4 builder (via local service).
mkdir -p \
    "$OVL_TMP/etc/local.d" \
    "$OVL_TMP/etc/runlevels/sysinit" \
    "$OVL_TMP/etc/runlevels/boot" \
    "$OVL_TMP/etc/runlevels/default" \
    "$OVL_TMP/etc/apk"

# Standard Alpine sysinit+boot services that the LIVE ISO enables by
# default but that the "install to new root" diskless flow doesn't
# set up on its own. modloop is the critical one — without it
# 9p-virtio module never loads and we can't reach the host share.
for svc in devfs dmesg mdev hwdrivers modloop; do
    ln -sf /etc/init.d/"$svc" "$OVL_TMP/etc/runlevels/sysinit/$svc"
done
for svc in bootmisc hostname hwclock modules sysctl syslog urandom; do
    ln -sf /etc/init.d/"$svc" "$OVL_TMP/etc/runlevels/boot/$svc"
done

# /etc/apk/world — packages Alpine's diskless-init will install to
# the new root before pivot. All available from the CDROM-backed
# local repo /media/cdrom/apks (the alpine-virt ISO ships them).
# attr + acl aren't in the virt ISO — those are installed later,
# in the local.d wrapper, via `apk add --allow-untrusted` against
# the .apk files we dropped on the 9p share.
cat > "$OVL_TMP/etc/apk/world" <<'PKGS_EOF'
alpine-base
busybox
e2fsprogs
e2fsprogs-extra
PKGS_EOF

# Single repo: the CDROM's local apk cache. Fully offline —
# apk never hits the network during "Install packages to root".
cat > "$OVL_TMP/etc/apk/repositories" <<'REPO_EOF'
/media/cdrom/apks
REPO_EOF

# Wrapper that chains the real builder (which lives on the 9p host
# share, so we don't bake it into the apkovl). Writes a done-marker
# back to the host so the watchdog knows the guest finished cleanly.
cat > "$OVL_TMP/etc/local.d/99-ext4.start" <<'WRAPPER_EOF'
#!/bin/sh
# Mirror everything to the console so a boot log captures it — the
# vm-build.log on the 9p share is only available after the mount
# succeeds, and debugging a silent failure is painful.
exec > /dev/console 2>&1

echo "=== [vm] local.d starting ==="

# Modules are baked into the virt kernel (9p, 9pnet, 9pnet_virtio,
# loop) so modprobe is mostly decorative, but harmless if missing.
modprobe 9p 9pnet 9pnet_virtio loop 2>/dev/null || true

mkdir -p /host
if ! mount -t 9p -o trans=virtio,version=9p2000.L,msize=131072 host /host; then
    echo "=== [vm] 9p mount failed — aborting ==="
    poweroff -f
fi
echo "=== [vm] /host mounted ==="

# .apk files are tar.gz archives of filesystem paths (plus a few
# `.PKGINFO` / `.SIGN.*` metadata entries at the top). For a
# one-shot VM, we don't need apk's package-db bookkeeping — just
# extract the relevant binaries + shared libs in-place.
for pkg in /host/.vm-cache/extra-apks/*.apk; do
    echo "=== [vm] extracting $(basename "$pkg") ==="
    tar -xzf "$pkg" -C / --exclude=.PKGINFO --exclude=.SIGN.\* \
        --exclude=.pre-install --exclude=.post-install \
        --exclude=.pre-upgrade --exclude=.post-upgrade 2>/dev/null || true
done

echo "=== [vm] running _vm-builder.sh ==="
if sh /host/_vm-builder.sh $(cat /host/.vm-cache/vm-args 2>/dev/null) \
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
chmod +x "$OVL_TMP/etc/local.d/99-ext4.start"

# Enable the `local` service so OpenRC runs /etc/local.d/*.start at boot.
ln -sf /etc/init.d/local "$OVL_TMP/etc/runlevels/default/local"

# Apkovl: Alpine auto-applies a .tar.gz file named
# `${HOSTNAME}.apkovl.tar.gz` (default HOSTNAME=localhost) found on
# any mounted filesystem at boot. We wrap it inside a tiny ISO9660
# image so qemu can attach it as a 2nd virtual CDROM — Alpine's
# init will mount the CDROM and apply the overlay without any
# `apkovl=` kernel cmdline argument.
OVL_STAGE="$CACHE/ovl-iso-stage"
rm -rf "$OVL_STAGE" "$CACHE/ovl.iso" "$CACHE/vm-build.done" "$CACHE/vm-build.failed" "$CACHE/vm-build.log"
mkdir -p "$OVL_STAGE"
(cd "$OVL_TMP" && tar -czf "$OVL_STAGE/localhost.apkovl.tar.gz" etc)
bsdtar -c -f "$CACHE/ovl.iso" --format=iso9660 -C "$OVL_STAGE" .

# (No HTTP server needed — the apkovl rides in on the 2nd CDROM.)

# ---------------------------------------------------------------------------
# Step 4 — boot Alpine under qemu with a 9p share of this directory.
# ---------------------------------------------------------------------------
echo "[host] booting Alpine under qemu (serial -> stdout)..."

# Pass the requested image-name list through to the guest by storing
# it on the 9p share — the local.d wrapper reads it back.
printf '%s\n' "$@" > "$CACHE/vm-args"

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
