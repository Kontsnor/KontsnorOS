#!/bin/bash
# KontsnorOS — Run kernel in QEMU with Ubuntu rootfs
#
# Usage: ./tools/run-ubuntu-qemu.sh [--release] [--debug]
#
# Options:
#   --release  Use the release build
#   --debug    Enable GDB debugging (port 1234)

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Parse arguments
BUILD_TYPE="release"
GDB_FLAG=""
QEMU_ARGS=()

for arg in "$@"; do
    case "$arg" in
        --release)
            BUILD_TYPE="release"
            ;;
        --debug)
            GDB_FLAG="-s"
            echo "GDB server will listen on localhost:1234"
            echo "Connect with: gdb -ex 'target remote :1234'"
            ;;
        *)
            QEMU_ARGS+=("$arg")
            ;;
    esac
done

KERNEL_BIN="$PROJECT_DIR/target/x86_64-unknown-none/$BUILD_TYPE/kontsnor-kernel"
BIOS_IMG="$PROJECT_DIR/bios.img"

if [ ! -f "$BIOS_IMG" ]; then
    echo "Bootable bios image not found at: $BIOS_IMG"
    echo "Building bootable kernel image first..."
    if [ "$BUILD_TYPE" = "release" ]; then
        "$PROJECT_DIR/tools/build-image.sh" --release
    else
        "$PROJECT_DIR/tools/build-image.sh"
    fi
fi

DISK_IMG="$PROJECT_DIR/disk-ubuntu.img"
if [ ! -f "$DISK_IMG" ]; then
    echo "Ubuntu disk image not found at: $DISK_IMG"
    echo "Formatting Ubuntu disk image first..."
    "$PROJECT_DIR/tools/format-ubuntu-disk.sh"
fi

echo "╔═══════════════════════════════════════╗"
echo "║  KontsnorOS — Ubuntu QEMU Launcher    ║"
echo "╠═══════════════════════════════════════╣"
echo "║  Build:  $BUILD_TYPE                  ║"
echo "║  Kernel: $KERNEL_BIN                  ║"
echo "║  Disk:   $DISK_IMG            ║"
echo "║  Serial: stdio                        ║"
echo "╚═══════════════════════════════════════╝"
echo ""

ACCEL_OPTS="-cpu qemu64,+fsgsbase -smp 8"
if [ -w /dev/kvm ] && qemu-system-x86_64 -enable-kvm -cpu host -M none -display none 2>/dev/null; then
    echo "Enabling KVM Hardware Acceleration (-enable-kvm -cpu host -smp 8)..."
    ACCEL_OPTS="-enable-kvm -cpu host -smp 8"
else
    echo "KVM unavailable, falling back to software TCG emulation (-cpu qemu64,+fsgsbase -smp 8)..."
fi
rm -f /tmp/qmp-kontsnor.sock

trap 'stty sane 2>/dev/null || true' EXIT INT TERM

qemu-system-x86_64 \
    -drive format=raw,file="$BIOS_IMG" \
    -drive format=raw,file="$DISK_IMG",index=1,media=disk \
    -chardev stdio,id=char0,signal=off \
    -serial chardev:char0 \
    -display none \
    -m 4G \
    -netdev user,id=net0 \
    -device e1000,netdev=net0 \
    -qmp unix:/tmp/qmp-kontsnor.sock,server,nowait \
    $ACCEL_OPTS \
    -no-reboot \
    -no-shutdown \
    $GDB_FLAG \
    "${QEMU_ARGS[@]}"
