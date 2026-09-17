#!/usr/bin/env bash
#
# vm-sh-slot-follows-the-machine.sh — `vm.sh` holds the oracle slot
# exactly while a machine is running, whatever vagrant's commands
# returned (#106, #107, #108).
#
# `vm.sh` and `vm-slot.sh` run from a sandbox copy of this repository's
# layout, against a stub `vagrant` whose machine state is a file: `up`,
# `halt` and `destroy -f` set it and exit with whatever each case asks,
# and `status --machine-readable` reports it. The assertion is always on
# the slot -- is the lock there, and is it this repository's -- because
# that is what another repository's `acquire` reads.
#
#   bash tests/scripts/vm-sh-slot-follows-the-machine.sh
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fails=0
sandbox="$(mktemp -d)"
trap 'rm -rf "$sandbox"' EXIT

repo="$sandbox/repo"
mkdir -p "$repo/scripts" "$repo/tests/vagrant/debian" "$sandbox/bin"
cp "$REPO/scripts/vm.sh" "$REPO/scripts/vm-slot.sh" "$repo/scripts/"
export AM_ORACLE_VM_STATE="$sandbox/state"
LOCK="$AM_ORACLE_VM_STATE/slot.lock"
machine="$sandbox/machine"

cat > "$sandbox/bin/vagrant" <<STUB
#!/usr/bin/env bash
case "\$1" in
    status) printf '1,default,state,%s\n' "\$(cat "$machine")" ;;
    up)      echo "\${UP_LEAVES:-running}" > "$machine"; exit "\${UP_EXIT:-0}" ;;
    halt)    echo "\${HALT_LEAVES:-poweroff}" > "$machine"; exit "\${HALT_EXIT:-0}" ;;
    destroy) echo "\${DESTROY_LEAVES:-not_created}" > "$machine"; exit "\${DESTROY_EXIT:-0}" ;;
esac
STUB
chmod +x "$sandbox/bin/vagrant"
export PATH="$sandbox/bin:$PATH"

vm() { env "$@" bash "$repo/scripts/vm.sh" "$VERB" >/dev/null 2>&1; }
ours() { bash "$repo/scripts/vm-slot.sh" holds; }
held() { [ -d "$LOCK" ]; }
start() { rm -rf "$AM_ORACLE_VM_STATE"; echo "$1" > "$machine"; }

check() {
    local what="$1"; shift
    if "$@"; then
        printf 'ok    %s\n' "$what"
    else
        printf 'FAIL  %s\n' "$what"
        fails=$((fails + 1))
    fi
}

VERB=up
start not_created; vm
check "up: a boot that succeeds holds the slot (control)" ours

# #106
start not_created; vm UP_EXIT=1 UP_LEAVES=running
check "up: a failed up that left the machine running keeps the slot" ours
start not_created; vm UP_EXIT=1 UP_LEAVES=not_created
check "up: a failed up with no machine hands the slot back" eval '! held'

# #107
start running; vm
check "up: a machine already running with the slot free is enrolled" ours
vm
check "up: and asking again while it is ours does not wait on itself" ours
start running
mkdir -p "$LOCK"
printf '%s\t%s\t%s\n' "/some/other/repo/tests/vagrant" "other" "$(date +%s)" > "$LOCK/holder"
( exec -a "qemu-system-stand-in -drive file=/some/other/repo/tests/vagrant/disk.img" sleep 30 ) &
other_vm=$!
sleep 0.2
if bash "$repo/scripts/vm.sh" up >/dev/null 2>&1; then
    printf 'FAIL  up: a running machine was accepted while a live VM elsewhere holds the slot\n'
    fails=$((fails + 1))
else
    printf 'ok    up: a running machine while a live VM elsewhere holds the slot fails at once\n'
fi
kill "$other_vm" 2>/dev/null; wait "$other_vm" 2>/dev/null

# #108
VERB=destroy
start running; bash "$repo/scripts/vm-slot.sh" acquire >/dev/null 2>&1
vm DESTROY_EXIT=1 DESTROY_LEAVES=not_created
check "destroy: a destroy that complained but removed the machine hands the slot back" eval '! held'
start running; bash "$repo/scripts/vm-slot.sh" acquire >/dev/null 2>&1
vm DESTROY_EXIT=1 DESTROY_LEAVES=running
check "destroy: a destroy that left the machine running keeps the slot" ours
start running; bash "$repo/scripts/vm-slot.sh" acquire >/dev/null 2>&1
if env DESTROY_EXIT=1 DESTROY_LEAVES=not_created bash "$repo/scripts/vm.sh" destroy >/dev/null 2>&1; then
    printf 'FAIL  destroy: a failed destroy reported success\n'; fails=$((fails + 1))
else
    printf 'ok    destroy: and a failed destroy still fails\n'
fi

VERB=down
start running; bash "$repo/scripts/vm-slot.sh" acquire >/dev/null 2>&1
vm HALT_LEAVES=running
check "down: a halt that left the machine running keeps the slot (unchanged)" ours
start running; bash "$repo/scripts/vm-slot.sh" acquire >/dev/null 2>&1
vm
check "down: a halt that stopped it hands the slot back (unchanged)" eval '! held'

if [ "$fails" -eq 0 ]; then
    echo "PASS  vm.sh slot follows the machine"
else
    echo "FAIL  $fails check(s)" >&2
    exit 1
fi
