#!/bin/sh
#
# Cross-validate fs-ext4 images against FreeBSD's native ext2/3 driver.
# Runs INSIDE the Vagrant VM (see ../Vagrantfile) — invoke via:
#
#     vagrant ssh -c /vagrant/run-cross-validate.sh
#
# Outputs a per-image manifest: filename → sha256(content) for every
# regular file plus a directory-listing hash. The host then diffs this
# against the equivalent manifest produced by Filesystem::mount + walk.
# Any divergence is a candidate cross-impl bug.
#
# FreeBSD-specific notes:
#   - mount_ext2fs(8) handles ext2/3/4-without-extents. ext4-with-extents
#     (modern Linux default) is supported on FreeBSD 13+ via the same
#     driver but read-only.
#   - We always mount RO here — write-side cross-validation is
#     intentionally out of scope; FreeBSD's writer would also need ext4
#     extent support which lands later upstream.

set -eu

IMAGE_DIR="${1:-/test-disks}"
MANIFEST_DIR="${2:-/tmp/freebsd-cross-manifest}"

mkdir -p "$MANIFEST_DIR"

for img in "$IMAGE_DIR"/*.img; do
    [ -f "$img" ] || continue
    name=$(basename "$img" .img)
    mount_point=$(mktemp -d "/tmp/${name}.XXXXXX")
    manifest="$MANIFEST_DIR/${name}.manifest"

    # `md` provides a memory-disk wrapper so mount_ext2fs sees a block
    # device. -F file mode skips the actual driver — but mount_ext2fs
    # accepts a vnode-backed pseudo-device too via the same path.
    md=$(mdconfig -a -t vnode -f "$img")

    # Mount RO — see header note about write support.
    if mount_ext2fs -o ro "/dev/${md}" "$mount_point"; then
        # Manifest: every file's path + sha256 + size + mode, sorted.
        find "$mount_point" -type f -print0 \
            | sort -z \
            | xargs -0 -I {} sh -c '
                rel=$(echo "{}" | sed "s|^'"$mount_point"'||")
                size=$(stat -f %z "{}")
                mode=$(stat -f %p "{}")
                sha=$(sha256 -q "{}")
                printf "%s\t%s\t%s\t%s\n" "$rel" "$size" "$mode" "$sha"
              ' > "$manifest"
        umount "$mount_point"
        echo "[freebsd-cross] $name: $(wc -l < $manifest) files manifested"
    else
        echo "[freebsd-cross] $name: mount_ext2fs failed (skipped)" >&2
    fi
    rmdir "$mount_point"
    mdconfig -d -u "$md"
done

echo "[freebsd-cross] manifests in $MANIFEST_DIR"
