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
# Images FreeBSD is KNOWN not to mount, by name without `.img`,
# space-separated. A refusal is a correct outcome for some images (see
# the notes above), so it is held to this list rather than either
# ignored or always failed: ignored, every image can be refused and the
# run still ends in success; always failed, a correct run is red and the
# script gets switched off.
EXPECTED_REFUSALS="${FREEBSD_EXPECTED_REFUSALS:-}"

mkdir -p "$MANIFEST_DIR"

expected_refusal() {
    for r in $EXPECTED_REFUSALS; do
        [ "$r" = "$1" ] && return 0
    done
    return 1
}

# Said on stdout AND stderr: a caller capturing only stdout must not see
# an unbroken run of success lines around a failure.
problem() {
    echo "[freebsd-cross] $1"
    echo "[freebsd-cross] $1" >&2
    failed="$failed
  $1"
}

images=0
manifested=0
refused=0
failed=""

for img in "$IMAGE_DIR"/*.img; do
    [ -f "$img" ] || continue
    images=$((images + 1))
    name=$(basename "$img" .img)
    # The name is not part of the mount point, and neither reaches a
    # shell string: both are passed to `sh -c` as arguments below. The
    # image's basename used to be interpolated into a `sed` expression
    # inside single quotes, which a name carrying `'` or `|` rewrote.
    mount_point=$(mktemp -d "/tmp/freebsd-cross.XXXXXX")
    manifest="$MANIFEST_DIR/${name}.manifest"
    partial="$manifest.partial"
    rm -f "$manifest" "$partial"

    # `md` provides a memory-disk wrapper so mount_ext2fs sees a block
    # device. -F file mode skips the actual driver — but mount_ext2fs
    # accepts a vnode-backed pseudo-device too via the same path.
    md=$(mdconfig -a -t vnode -f "$img")

    # Mount RO — see header note about write support.
    if mount_ext2fs -o ro "/dev/${md}" "$mount_point"; then
        # Manifest: every file's path + size + mode + sha256, sorted.
        #
        # A FAILURE PART-WAY IS NOT A SHORTER MANIFEST. Each file's line
        # comes from its own `sh`, which exits 255 when any of its three
        # commands fails; that stops `xargs`, whose status is then
        # non-zero, and the partial output is discarded rather than
        # renamed into place.
        if find "$mount_point" -type f -print0 \
            | sort -z \
            | xargs -0 -n 1 sh -c '
                # GNU xargs runs this once with no file when there are
                # none; FreeBSD'"'"'s does not. Either way, no file, no line.
                [ -n "${2:-}" ] || exit 0
                f=$2
                rel=${f#"$1"}
                size=$(stat -f %z "$f") || exit 255
                mode=$(stat -f %p "$f") || exit 255
                sha=$(sha256 -q "$f") || exit 255
                printf "%s\t%s\t%s\t%s\n" "$rel" "$size" "$mode" "$sha"
              ' sh "$mount_point" > "$partial"; then
            if [ -s "$partial" ]; then
                mv "$partial" "$manifest"
                manifested=$((manifested + 1))
                echo "[freebsd-cross] $name: $(wc -l < "$manifest") files manifested"
            else
                rm -f "$partial"
                problem "$name: mounted, and no files were manifested"
            fi
        else
            rm -f "$partial"
            problem "$name: the manifest could not be built for every file"
        fi
        umount "$mount_point"
    elif expected_refusal "$name"; then
        refused=$((refused + 1))
        echo "[freebsd-cross] $name: mount_ext2fs refused it, as expected"
    else
        problem "$name: mount_ext2fs refused it, and it is not an expected refusal"
    fi
    rmdir "$mount_point"
    mdconfig -d -u "$md"
done

echo "[freebsd-cross] $images images: $manifested manifested, $refused refused as expected"
if [ "$images" -eq 0 ]; then
    problem "no images in $IMAGE_DIR"
fi
if [ -n "$failed" ]; then
    echo "[freebsd-cross] FAIL:$failed" >&2
    exit 1
fi
echo "[freebsd-cross] manifests in $MANIFEST_DIR"
