#!/usr/bin/env bash
# Architecture selection shared by the fixture generator and its shell tests.
# Source this file; it normalises EXT4_VM_ARCH and defines the QEMU settings
# needed to boot an Alpine guest of that architecture. Native guests require
# hardware acceleration; cross-architecture TCG is available only through the
# explicit EXT4_VM_ALLOW_TCG=1 diagnostic override.

case "$(uname -m)" in
    x86_64|amd64) HOST_VM_ARCH=x86_64 ;;
    aarch64|arm64) HOST_VM_ARCH=aarch64 ;;
    *) HOST_VM_ARCH=unsupported ;;
esac

case "${EXT4_VM_ARCH:-$(uname -m)}" in
    x86_64|amd64)
        EXT4_VM_ARCH=x86_64
        ALPINE_ARCH=x86_64
        QEMU_SYSTEM=qemu-system-x86_64
        QEMU_DRIVE_IF=ide
        QEMU_CONSOLE=ttyS0
        QEMU_BOOT_MODE=direct
        QEMU_FIRMWARE=""
        QEMU_MACHINE_ARGS=()
        ;;
    aarch64|arm64)
        EXT4_VM_ARCH=aarch64
        ALPINE_ARCH=aarch64
        QEMU_SYSTEM=qemu-system-aarch64
        QEMU_DRIVE_IF=virtio
        QEMU_CONSOLE=ttyAMA0
        QEMU_BOOT_MODE=uefi
        QEMU_FIRMWARE="${EXT4_QEMU_EFI:-}"
        QEMU_MACHINE_ARGS=(-machine virt)
        ;;
    *)
        echo "unsupported fixture VM architecture: ${EXT4_VM_ARCH:-$(uname -m)}" >&2
        echo "supported values: x86_64, amd64, aarch64, arm64" >&2
        return 2 2>/dev/null || exit 2
        ;;
esac

QEMU_ACCEL=""
if [[ "$EXT4_VM_ARCH" == "$HOST_VM_ARCH" ]]; then
    case "$(uname -s)" in
        Darwin)
            QEMU_ACCEL=hvf
            ;;
        Linux)
            if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
                QEMU_ACCEL=kvm
            fi
            ;;
    esac
fi

if [[ -z "$QEMU_ACCEL" ]]; then
    if [[ "${EXT4_VM_ALLOW_TCG:-0}" == "1" ]]; then
        QEMU_ACCEL=tcg
    else
        echo "hardware acceleration is unavailable for an $EXT4_VM_ARCH fixture VM on $(uname -s)/$HOST_VM_ARCH" >&2
        echo "use a native guest, enable KVM/HVF, or set EXT4_VM_ALLOW_TCG=1 for deliberate slow emulation" >&2
        return 2 2>/dev/null || exit 2
    fi
fi

QEMU_MACHINE_ARGS+=(-accel "$QEMU_ACCEL")
if [[ "$QEMU_ACCEL" == "tcg" ]]; then
    [[ "$EXT4_VM_ARCH" == "aarch64" ]] && QEMU_MACHINE_ARGS+=(-cpu max)
else
    QEMU_MACHINE_ARGS+=(-cpu host)
fi

if [[ "$QEMU_BOOT_MODE" == "uefi" && -z "$QEMU_FIRMWARE" ]]; then
    qemu_bin="$(command -v "$QEMU_SYSTEM" 2>/dev/null || true)"
    for candidate in \
        /usr/share/qemu-efi-aarch64/QEMU_EFI.fd \
        /usr/share/AAVMF/AAVMF_CODE.fd \
        /opt/homebrew/share/qemu/edk2-aarch64-code.fd \
        /usr/local/share/qemu/edk2-aarch64-code.fd \
        "${qemu_bin:+$(dirname "$qemu_bin")/../share/qemu/edk2-aarch64-code.fd}"
    do
        if [[ -n "$candidate" && -f "$candidate" ]]; then
            QEMU_FIRMWARE="$candidate"
            break
        fi
    done
fi

export EXT4_VM_ARCH ALPINE_ARCH QEMU_SYSTEM QEMU_DRIVE_IF QEMU_CONSOLE QEMU_ACCEL
export QEMU_BOOT_MODE QEMU_FIRMWARE
