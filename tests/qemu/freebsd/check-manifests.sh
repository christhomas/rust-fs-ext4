#!/usr/bin/env bash
#
# check-manifests.sh — recover the per-image manifests from the FreeBSD
# guest's serial log, and fail unless every attached image is accounted
# for.
#
#   check-manifests.sh SERIAL_LOG MANIFEST_DIR IMAGE_NAME...
#
# IMAGE_NAME... are the attached images in the order the host attached
# them: the first is the guest's `vdb`, the next `vdc`, and so on, which
# is how `user-data` names each disk in its markers.
#
# WHY A SEPARATE SCRIPT. `run-cross-validate.sh` printed DONE having
# checked only that the guest reached `[manifest:end]`. A log with that
# marker and no disk blocks produced zero manifests and exit 0, and
# nothing compared the number of manifests with the number of images
# attached (#171). Separate, so the check can be tested without booting
# a VM.
#
# An image FreeBSD cannot mount is a correct outcome for some images,
# so a refusal is held to FREEBSD_EXPECTED_REFUSALS (image names without
# `.img`, space-separated) rather than ignored or always failed.
set -euo pipefail

[ "$#" -ge 2 ] || { echo "usage: check-manifests.sh SERIAL_LOG MANIFEST_DIR IMAGE_NAME..." >&2; exit 2; }
log="$1"
dir="$2"
shift 2

if ! grep -q '\[manifest:end\]' "$log"; then
    echo "[qemu-fbsd] FAIL: cloud-init didn't reach manifest:end. Tail of serial log:"
    tail -40 "$log"
    exit 1
fi
if [ "$#" -eq 0 ]; then
    echo "[qemu-fbsd] FAIL: no images were attached, so there is nothing to validate"
    exit 1
fi

mkdir -p "$dir"
echo "[qemu-fbsd] manifests:"
awk -v dir="$dir" '
    /^\[manifest:disk:.*:begin\]$/ {
        match($0, /disk:[^:]+/)
        name = substr($0, RSTART+5, RLENGTH-5)
        out = dir "/" name ".manifest"
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
' "$log"

expected=" ${FREEBSD_EXPECTED_REFUSALS:-} "
letters=(b c d e f g h i j k l m n o p q r s t u v w x y z)
if [ "$#" -gt "${#letters[@]}" ]; then
    echo "[qemu-fbsd] FAIL: $# images attached, more than the guest's vdb..vdz names"
    exit 1
fi
failed=()
manifested=0
refused=0
i=0
for name in "$@"; do
    dev="vd${letters[$i]}"
    i=$((i + 1))
    if grep -qxF "[manifest:disk:${dev}:end:ok]" "$log"; then
        if [ -s "$dir/$dev.manifest" ]; then
            manifested=$((manifested + 1))
            echo "  $name is $dev: $(wc -l < "$dir/$dev.manifest") files"
        else
            failed+=("$name ($dev): mounted, and no files were manifested")
        fi
    elif grep -qxF "[manifest:disk:${dev}:end:mount_failed]" "$log"; then
        if [[ "$expected" == *" $name "* ]]; then
            refused=$((refused + 1))
            echo "  $name is $dev: refused, as expected"
        else
            failed+=("$name ($dev): FreeBSD refused to mount it, and it is not an expected refusal")
        fi
    else
        failed+=("$name ($dev): the guest reported no result for it")
    fi
done

echo "[qemu-fbsd] $# images attached: $manifested manifested, $refused refused as expected"
if [ "${#failed[@]}" -gt 0 ]; then
    printf '[qemu-fbsd] FAIL: %s\n' "${failed[@]}"
    exit 1
fi
