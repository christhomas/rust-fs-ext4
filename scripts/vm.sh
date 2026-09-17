#!/usr/bin/env bash
#
# vm.sh — drive the Debian arm64 oracle VM.
#
#   vm.sh up            boot the VM (idempotent; provisions on first run)
#   vm.sh run <cmd...>  run a command inside the VM
#   vm.sh share         print the host path of the shared directory
#   vm.sh put <file>    copy a file into the shared directory, echo guest path
#   vm.sh down          halt the VM (state is kept; next `up` is fast)
#   vm.sh destroy       delete the VM entirely
#
# The VM is the real-Linux oracle: mkfs.xfs, the in-kernel XFS driver and
# xfs_repair are Linux-only, and validating this driver against anything
# less than a real kernel would just be marking our own homework.
#
# The VM is kept running between invocations on purpose. Booting is the
# slow part; an iterate-and-check loop should pay it once.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VAGRANT_DIR="$REPO/tests/vagrant/debian"
SHARE="$REPO/.vm-share"
# One oracle VM runs at a time, across every repository — see
# scripts/vm-slot.sh for why. Absent (an older checkout, a partial
# copy), everything below still works and the serialisation is simply
# not enforced; a missing helper should not stop a developer building
# fixtures.
SLOT="$REPO/scripts/vm-slot.sh"

mkdir -p "$SHARE"

machine_running() {
    (cd "$VAGRANT_DIR" && vagrant status --machine-readable 2>/dev/null \
        | grep -q ',state,running')
}

# Hand the slot back only when no machine is running. A command that
# failed, or even succeeded, says nothing reliable about that; the state
# does. Returns 1, keeping the slot, while one runs.
release_if_down() {
    if machine_running; then
        echo "vm: $1, and the machine is still running." >&2
        echo "    The oracle slot is deliberately kept; \`vm.sh destroy\` reclaims it." >&2
        return 1
    fi
    [ -x "$SLOT" ] && "$SLOT" release || true
}

vm_up() {
    # `vagrant status` is authoritative but slow-ish; only boot when the
    # machine is not already running.
    if machine_running; then
        # RUNNING IS NOT THE SAME AS ENROLLED (#107). A machine can be up
        # with the slot free: booted before this repository had the slot
        # helper, by hand with `vagrant up`, or after its lock was broken.
        # Take the slot for it now if it is free -- without waiting, since
        # a second machine is already up if someone else holds it -- and
        # do not ask again when it is already ours, which would wait on
        # itself.
        if [ -x "$SLOT" ] && ! "$SLOT" holds; then
            AM_ORACLE_VM_WAIT=0 "$SLOT" acquire || {
                echo "vm: this machine is running while another repository holds the oracle slot." >&2
                echo "    Two VMs may be up. Stop one: \`vm.sh down\` here, or theirs." >&2
                exit 1
            }
        fi
        return 0
    fi
    # TAKE THE SLOT BEFORE BOOTING.
    if [ -x "$SLOT" ]; then
        "$SLOT" acquire || {
            echo "vm: could not get the oracle slot; not booting a second VM." >&2
            exit 1
        }
    fi
    if ! (cd "$VAGRANT_DIR" && vagrant up); then
        # A FAILED `up` CAN LEAVE A MACHINE RUNNING (#106): it boots and
        # then provisions, so a failing provisioner, an interrupted run or
        # a failed synced-folder mount returns nonzero with the guest up.
        # The slot goes back only if nothing is running, as `down` does.
        release_if_down "vagrant up failed" || true
        exit 1
    fi
}

case "${1:-}" in
    up)
        vm_up
        ;;
    run)
        shift
        vm_up
        # `vagrant ssh -c` mangles quoting for complex commands; feed the
        # command on stdin instead so the guest shell sees it verbatim.
        printf '%s\n' "$*" | (cd "$VAGRANT_DIR" && vagrant ssh -- -T 'sudo bash -s')
        ;;
    share)
        echo "$SHARE"
        ;;
    put)
        [ $# -eq 2 ] || { echo "usage: vm.sh put <file>" >&2; exit 2; }
        cp "$2" "$SHARE/"
        echo "/share/$(basename "$2")"
        ;;
    down)
        # CONFIRM BEFORE RELEASING. `vagrant halt` reporting success is
        # not the same as the machine being down, and a slot handed back
        # while the VM still runs lets the next repository boot a second
        # one beside it — the exact thing this serialisation exists to
        # prevent.
        halted=0
        (cd "$VAGRANT_DIR" && vagrant halt) || halted=$?
        release_if_down "halt did not stop the machine" || exit 1
        exit "$halted"
        ;;
    destroy)
        # The release used to be unreachable whenever `destroy -f` exited
        # nonzero, `set -e` ending the script first, and unconditional when
        # it was reached -- where a destroy that failed can leave the
        # machine up (#108). It now follows the machine's state either way,
        # and a failed destroy still fails.
        destroyed=0
        (cd "$VAGRANT_DIR" && vagrant destroy -f) || destroyed=$?
        release_if_down "destroy did not remove the machine" || exit 1
        exit "$destroyed"
        ;;
    *)
        sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
        exit 2
        ;;
esac
