#!/usr/bin/env bash
#
# freebsd-manifests-fail-closed.sh — the FreeBSD cross-validator's two
# manifest steps fail when an image is not accounted for (#171).
#
# Neither needs FreeBSD to test. The in-guest producer runs here with
# `mdconfig`, `mount_ext2fs`, `umount`, `stat -f` and `sha256` stubbed,
# the "mounted" tree copied from a per-image directory. The host check
# reads a synthetic serial log.
#
#   bash tests/scripts/freebsd-manifests-fail-closed.sh
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PRODUCER="$REPO/tests/vagrant/freebsd/run-cross-validate.sh"
CHECK="$REPO/tests/qemu/freebsd/check-manifests.sh"
fails=0
t="$(mktemp -d)"
trap 'rm -rf "$t"' EXIT

expect() {
    local want="$1" got="$2" what="$3"
    if [ "$got" = "$want" ]; then
        printf 'ok    %s\n' "$what"
    else
        printf 'FAIL  %s: exit %s, expected %s\n' "$what" "$got" "$want"
        sed 's/^/      /' "$t/out"
        fails=$((fails + 1))
    fi
}

# --- the in-guest producer ----------------------------------------------

mkdir -p "$t/bin" "$t/trees"
cat > "$t/bin/mdconfig" <<'STUB'
#!/usr/bin/env bash
# -a -t vnode -f IMG  ->  a device named after the image; -d -u MD  ->  ok
[ "$1" = "-a" ] && { printf "md-%s\n" "$(basename "$5" .img)"; }
exit 0
STUB
cat > "$t/bin/mount_ext2fs" <<'STUB'
#!/usr/bin/env bash
# -o ro /dev/md-NAME MOUNTPOINT: "mount" the tree $TREES/NAME, or refuse.
name="${3#/dev/md-}"
[ -d "$TREES/$name" ] || exit 1
cp -R "$TREES/$name/." "$4/"
STUB
printf '#!/usr/bin/env bash\nfind "$1" -mindepth 1 -delete\n' > "$t/bin/umount"
cat > "$t/bin/stat" <<'STUB'
#!/usr/bin/env bash
# FreeBSD's `stat -f FORMAT FILE`, for the two formats the producer uses.
[ -f "$3" ] || exit 1
case "$2" in %z) wc -c < "$3" | tr -d " " ;; %p) echo 100644 ;; esac
STUB
cat > "$t/bin/sha256" <<'STUB'
#!/usr/bin/env bash
# `sha256 -q FILE`; FAIL_SHA names a file whose digest cannot be taken.
[ -n "${FAIL_SHA:-}" ] && [ "$(basename "$2")" = "$FAIL_SHA" ] && exit 1
sha256sum "$2" | cut -d' ' -f1
STUB
# `sort -z`, failing after writing its output when FAIL_SORT is set: the
# stage that a pipeline's last status did not see.
real_sort="$(command -v sort)"
cat > "$t/bin/sort" <<STUB
#!/usr/bin/env bash
"$real_sort" "\$@"
[ -z "\${FAIL_SORT:-}" ]
STUB
chmod +x "$t/bin/"*

produce() {
    rm -rf "$t/img" "$t/manifests"
    mkdir -p "$t/img"
    local name
    for name in "$@"; do : > "$t/img/$name.img"; done
    env PATH="$t/bin:$PATH" TREES="$t/trees" ${ENVS[@]+"${ENVS[@]}"} \
        sh "$PRODUCER" "$t/img" "$t/manifests" > "$t/out" 2>&1
}

mkdir -p "$t/trees/good/sub" "$t/trees/empty" "$t/trees/we'ird|name"
echo one > "$t/trees/good/a.txt"
echo two > "$t/trees/good/sub/b.txt"
echo three > "$t/trees/we'ird|name/c.txt"

ENVS=()
produce good "we'ird|name"; expect 0 "$?" "producer: every image manifested (control)"
if [ "$(wc -l < "$t/manifests/good.manifest")" = 2 ] \
    && grep -q "^/c.txt	" "$t/manifests/we'ird|name.manifest"; then
    printf 'ok    and each manifest holds its files, a quote and a pipe in the name notwithstanding\n'
else
    printf 'FAIL  manifests wrong:\n'; cat "$t/manifests/"*.manifest; fails=$((fails + 1))
fi

produce good refused-image; expect 1 "$?" "producer: an unexpected refusal fails"
if grep -q "refused-image" <(env PATH="$t/bin:$PATH" TREES="$t/trees" sh "$PRODUCER" "$t/img" "$t/m2" 2>/dev/null); then
    printf 'ok    and the refusal is on stdout too\n'
else
    printf 'FAIL  the refusal is missing from stdout\n'; fails=$((fails + 1))
fi

ENVS=(FREEBSD_EXPECTED_REFUSALS="other refused-image")
produce good refused-image; expect 0 "$?" "producer: an expected refusal passes"
if env PATH="$t/bin:$PATH" TREES="$t/trees" FREEBSD_EXPECTED_REFUSALS="refused-image" \
    sh "$PRODUCER" "$t/img" "$t/m3" 2>&1 >/dev/null | grep -q "refused-image: .*as expected"; then
    printf 'ok    and the expected refusal is on stderr too\n'
else
    printf 'FAIL  the expected refusal is missing from stderr\n'; fails=$((fails + 1))
fi
ENVS=()

produce good empty; expect 1 "$?" "producer: a mounted image with no files fails"

ENVS=(FAIL_SHA=b.txt)
produce good; expect 1 "$?" "producer: a file whose digest failed fails the image"
if [ ! -e "$t/manifests/good.manifest" ]; then
    printf 'ok    and leaves no truncated manifest behind\n'
else
    printf 'FAIL  a truncated manifest was kept\n'; fails=$((fails + 1))
fi
ENVS=()

ENVS=(FAIL_SORT=1)
produce good; expect 1 "$?" "producer: a sort that fails after writing fails the image"
if [ ! -e "$t/manifests/good.manifest" ]; then
    printf 'ok    and publishes no manifest\n'
else
    printf 'FAIL  a manifest was published past a failed sort\n'; fails=$((fails + 1))
fi
ENVS=()

produce; expect 1 "$?" "producer: no images at all fails"

# --- the host check -------------------------------------------------------

check() {
    env ${ENVS[@]+"${ENVS[@]}"} bash "$CHECK" "$t/serial.log" "$t/hm" "$@" > "$t/out" 2>&1
}
rm -rf "$t/hm"
printf '%s\n' "[manifest:start]" \
    "[manifest:disk:vdb:begin]" "./a.txt	4	abc" "[manifest:disk:vdb:end:ok]" \
    "[manifest:disk:vdc:begin]" "[manifest:disk:vdc:end:mount_failed]" \
    "[manifest:end]" > "$t/serial.log"

check good; expect 0 "$?" "host: one image, manifested (control)"
check good refused; expect 1 "$?" "host: an unexpected refusal fails"
ENVS=(FREEBSD_EXPECTED_REFUSALS=refused)
check good refused; expect 0 "$?" "host: an expected refusal passes"
ENVS=()
check good refused third; expect 1 "$?" "host: an attached image the guest never reported fails"

printf '%s\n' "[manifest:start]" "[manifest:end]" > "$t/serial.log"
rm -rf "$t/hm"
check good; expect 1 "$?" "host: manifest:end with no disk blocks fails"

printf '%s\n' "[manifest:start]" "[manifest:disk:vdb:begin]" "[manifest:disk:vdb:end:ok]" "[manifest:end]" > "$t/serial.log"
rm -rf "$t/hm"
check good; expect 1 "$?" "host: an ok disk with no files fails"
check; expect 1 "$?" "host: no images attached fails"

# The same directory, reused: a good run, then a log whose ok block for the
# same disk carries no files. The earlier run's manifest must not pass it.
rm -rf "$t/hm"
printf '%s\n' "[manifest:start]" \
    "[manifest:disk:vdb:begin]" "./a.txt	4	abc" "[manifest:disk:vdb:end:ok]" \
    "[manifest:end]" > "$t/serial.log"
check good; expect 0 "$?" "host: reused directory, first run manifested (control)"
printf '%s\n' "[manifest:start]" "[manifest:disk:vdb:begin]" "[manifest:disk:vdb:end:ok]" "[manifest:end]" > "$t/serial.log"
check good; expect 1 "$?" "host: reused directory, a later ok block with no files still fails"

if [ "$fails" -eq 0 ]; then
    echo "PASS  freebsd manifests fail closed"
else
    echo "FAIL  $fails check(s)" >&2
    exit 1
fi
