#!/usr/bin/env bash
# Architecture selection shared by the fixture generator and its shell tests.
# Source this file; it normalises EXT4_VM_ARCH and defines the QEMU settings
# needed to boot an Alpine guest of that architecture.

case "${EXT4_VM_ARCH:-$(uname -m)}" in
    x86_64|amd64)
        EXT4_VM_ARCH=x86_64
        ALPINE_ARCH=x86_64
        QEMU_SYSTEM=qemu-system-x86_64
        QEMU_DRIVE_IF=ide
        QEMU_CONSOLE=ttyS0
        QEMU_MACHINE_ARGS=()
        ;;
    aarch64|arm64)
        EXT4_VM_ARCH=aarch64
        ALPINE_ARCH=aarch64
        QEMU_SYSTEM=qemu-system-aarch64
        QEMU_DRIVE_IF=virtio
        QEMU_CONSOLE=ttyAMA0
        QEMU_MACHINE_ARGS=(-machine virt)
        case "$(uname -s)" in
            Darwin)
                QEMU_MACHINE_ARGS+=(-accel hvf -cpu host)
                ;;
            Linux)
                if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
                    QEMU_MACHINE_ARGS+=(-accel kvm -cpu host)
                else
                    QEMU_MACHINE_ARGS+=(-cpu max)
                fi
                ;;
            *)
                QEMU_MACHINE_ARGS+=(-cpu max)
                ;;
        esac
        ;;
    *)
        echo "unsupported fixture VM architecture: ${EXT4_VM_ARCH:-$(uname -m)}" >&2
        echo "supported values: x86_64, amd64, aarch64, arm64" >&2
        return 2 2>/dev/null || exit 2
        ;;
esac

export EXT4_VM_ARCH ALPINE_ARCH QEMU_SYSTEM QEMU_DRIVE_IF QEMU_CONSOLE
