#!/usr/bin/env bash
#
# fixture-vm-architecture.sh — the fixture oracle follows the host ISA unless
# the caller deliberately requests another one.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
GENERATOR="$REPO/test-disks/build-ext4-feature-images.sh"
ARCH_HELPER="$REPO/test-disks/vm-architecture.sh"
fails=0

if [ ! -f "$ARCH_HELPER" ]; then
    echo "FAIL  fixture VM architecture helper is missing" >&2
    exit 1
fi

check_line() {
    local output="$1" expected="$2" what="$3"
    if printf '%s\n' "$output" | grep -Fxq "$expected"; then
        printf 'ok    %s\n' "$what"
    else
        printf 'FAIL  %s: expected %s in:\n%s\n' "$what" "$expected" "$output" >&2
        fails=$((fails + 1))
    fi
}

config_for() {
    EXT4_VM_ARCH="$1" EXT4_VM_ALLOW_TCG=1 bash "$GENERATOR" --print-vm-config
}

x86="$(config_for amd64)"
check_line "$x86" 'EXT4_VM_ARCH=x86_64' "amd64 normalises to x86_64"
check_line "$x86" 'ALPINE_ARCH=x86_64' "x86 selects Alpine x86_64 assets"
check_line "$x86" 'QEMU_SYSTEM=qemu-system-x86_64' "x86 selects the x86 system emulator"
check_line "$x86" 'QEMU_DRIVE_IF=ide' "x86 keeps the established IDE boot-media path"
check_line "$x86" 'QEMU_CONSOLE=ttyS0' "x86 keeps its serial console"

arm="$(config_for arm64)"
check_line "$arm" 'EXT4_VM_ARCH=aarch64' "arm64 normalises to aarch64"
check_line "$arm" 'ALPINE_ARCH=aarch64' "ARM selects Alpine aarch64 assets"
check_line "$arm" 'QEMU_SYSTEM=qemu-system-aarch64' "ARM selects the ARM system emulator"
check_line "$arm" 'QEMU_DRIVE_IF=virtio' "ARM uses boot media supported by the virt machine"
check_line "$arm" 'QEMU_CONSOLE=ttyAMA0' "ARM selects its serial console"
check_line "$arm" 'QEMU_BOOT_MODE=uefi' "ARM enters its EFI kernel through firmware"

# Standard GitHub ARM runners intentionally build fixtures with the native
# Linux host tools and do not expose /dev/kvm. Permit TCG while inspecting the
# default architecture choice; the separate conditional below still requires
# KVM when this host actually exposes it.
host="$(EXT4_VM_ALLOW_TCG=1 bash "$GENERATOR" --print-vm-config)"
case "$(uname -m)" in
    x86_64|amd64) expected_host=x86_64 ;;
    aarch64|arm64) expected_host=aarch64 ;;
    *) expected_host=unsupported ;;
esac
check_line "$host" "EXT4_VM_ARCH=$expected_host" "no override follows the local host architecture"

case "$(uname -s):$(uname -m)" in
    Linux:x86_64|Linux:amd64|Linux:aarch64|Linux:arm64)
        if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
            check_line "$host" 'QEMU_ACCEL=kvm' "native Linux uses KVM acceleration"
        else
            check_line "$host" 'QEMU_ACCEL=tcg' "configuration inspection explicitly permits TCG without KVM"
        fi
        ;;
    Darwin:arm64)
        check_line "$host" 'QEMU_ACCEL=hvf' "Apple Silicon uses HVF acceleration"
        ;;
esac

if grep -Fq '.vm-cache/vm-arch' "$GENERATOR"; then
    echo "FAIL  fixture generators share a mutable architecture selector" >&2
    fails=$((fails + 1))
else
    echo "ok    fixture architecture is embedded rather than shared through mutable state"
fi
for per_arch_state in 'ovl-$ALPINE_ARCH.iso' 'vm-args-$ALPINE_ARCH' 'vm-build-$ALPINE_ARCH.done'; do
    if grep -Fq "$per_arch_state" "$GENERATOR"; then
        printf 'ok    per-architecture state includes %s\n' "$per_arch_state"
    else
        printf 'FAIL  generator state is not architecture-specific: %s\n' "$per_arch_state" >&2
        fails=$((fails + 1))
    fi
done

if EXT4_VM_ARCH=mips64 bash "$GENERATOR" --print-vm-config > /dev/null 2>&1; then
    echo "FAIL  an unsupported architecture was accepted" >&2
    fails=$((fails + 1))
else
    echo "ok    an unsupported architecture is refused before VM setup"
fi

if [ "$fails" -eq 0 ]; then
    echo "PASS  fixture VM architecture selection"
else
    echo "FAIL  $fails assertion(s)" >&2
    exit 1
fi
