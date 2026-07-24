#!/bin/bash
# KontsnorOS — Run kernel in QEMU
#
# Usage: ./tools/run-qemu.sh [--release] [--debug]
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
    echo "Please build the image first: ./tools/build-image.sh"
    exit 1
fi

echo "╔═══════════════════════════════════════╗"
echo "║     KontsnorOS — QEMU Launcher        ║"
echo "╠═══════════════════════════════════════╣"
echo "║  Build:  $BUILD_TYPE                  ║"
echo "║  Kernel: $KERNEL_BIN                  ║"
echo "║  Image:  $BIOS_IMG                    ║"
echo "║  Serial: stdio                        ║"
echo "╚═══════════════════════════════════════╝"
echo ""

DISK_IMG="$PROJECT_DIR/disk.img"
if [ ! -f "$DISK_IMG" ]; then
    echo "Creating 1.5GB blank persistent hard drive image..."
    dd if=/dev/zero of="$DISK_IMG" bs=1M count=1536 2>/dev/null
fi

ACCEL_OPTS="-cpu qemu64,+fsgsbase -smp 1"
if [ -w /dev/kvm ]; then
    echo "Enabling KVM Hardware Acceleration (-enable-kvm -cpu host -smp 4)..."
    ACCEL_OPTS="-enable-kvm -cpu host -smp 4"
else
    echo "KVM unavailable, falling back to software TCG emulation..."
fi

qemu-system-x86_64 \
    -drive format=raw,file="$BIOS_IMG" \
    -drive format=raw,file="$DISK_IMG",index=1,media=disk \
    -serial stdio \
    -display none \
    -m 5G \
    $ACCEL_OPTS \
    -no-reboot \
    -no-shutdown \
    $GDB_FLAG \
    "${QEMU_ARGS[@]}"
