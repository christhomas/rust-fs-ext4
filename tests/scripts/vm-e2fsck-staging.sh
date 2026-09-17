#!/usr/bin/env bash
#
# vm-e2fsck-staging.sh — each image is checked under a name the script
# chose (#153).
#
# `vm-e2fsck.sh` used each image's basename as its staged name, inside a
# command string the guest shell runs as root. Two faults followed: a
# basename with shell syntax ran in the guest, and two inputs with one
# basename were staged over each other, so both checks read the second
# image and the first was reported as passing without being looked at.
#
# The VM is replaced by a stub: the script is copied into a sandbox
# layout beside a `vm.sh` whose `run` executes the command string with
# bash, as the guest does, against a stub `e2fsck` that records the
# checksum of the file it was given. So an injected command really runs,
# and a collision shows as a checksum missing from the record.
#
#   bash tests/scripts/vm-e2fsck-staging.sh
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fails=0
fail() { echo "FAIL: $*"; fails=$((fails + 1)); }

sandbox="$(mktemp -d)"
trap 'rm -rf "$sandbox"' EXIT
mkdir -p "$sandbox/scripts" "$sandbox/bin" "$sandbox/guest-cwd" \
    "$sandbox/in/a" "$sandbox/in/b" "$sandbox/in/c"
cp "$REPO/scripts/vm-e2fsck.sh" "$sandbox/scripts/"

cat > "$sandbox/scripts/vm.sh" <<STUB
#!/usr/bin/env bash
case "\$1" in
    up) ;;
    run)
        shift
        cmd="\$*"
        cd "$sandbox/guest-cwd"
        PATH="$sandbox/bin:\$PATH" bash -c "\${cmd//\/share\//$sandbox/.vm-share/}"
        ;;
esac
STUB
cat > "$sandbox/bin/e2fsck" <<STUB
#!/usr/bin/env bash
cksum < "\$2" | cut -d' ' -f1 >> "$sandbox/checked"
STUB
chmod +x "$sandbox/scripts/vm.sh" "$sandbox/bin/e2fsck"

printf 'first image' > "$sandbox/in/a/test.img"
printf 'second image' > "$sandbox/in/b/test.img"
evil='x;touch pwned;y.img'
printf 'third image' > "$sandbox/in/c/$evil"

out="$("$sandbox/scripts/vm-e2fsck.sh" "$sandbox/in/a/test.img" "$sandbox/in/b/test.img" \
    "$sandbox/in/c/$evil" 2>&1)" || fail "the run failed: $out"

for f in "$sandbox/in/a/test.img" "$sandbox/in/b/test.img" "$sandbox/in/c/$evil"; do
    sum="$(cksum < "$f" | cut -d' ' -f1)"
    grep -qx "$sum" "$sandbox/checked" 2>/dev/null ||
        fail "$f was never checked (a staged copy was overwritten or never made)"
    grep -qF "e2fsck $f " <<< "$out" || fail "the report does not name $f"
done
[ "$(wc -l < "$sandbox/checked")" -eq 3 ] || fail "expected three checks, got: $(cat "$sandbox/checked")"
[ ! -e "$sandbox/guest-cwd/pwned" ] || fail "a filename ran as a command in the guest"
leftover="$(ls -A "$sandbox/.vm-share")"
[ -z "$leftover" ] || fail "staged copies were left behind: $leftover"

if [ "$fails" -ne 0 ]; then
    echo "$fails failure(s)"
    exit 1
fi
echo "ok: vm-e2fsck.sh stages each image under its own safe name"
