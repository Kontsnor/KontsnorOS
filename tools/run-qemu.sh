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

for arg in "$@"; do
    case "$arg" in
        --release)
            BUILD_TYPE="release"
            ;;
        --debug)
            GDB_FLAG="-s -S"
            echo "GDB server will listen on localhost:1234"
            echo "Connect with: gdb -ex 'target remote :1234'"
            ;;
    esac
done

KERNEL_BIN="$PROJECT_DIR/target/x86_64-unknown-none/$BUILD_TYPE/kontsnor-kernel"

if [ ! -f "$KERNEL_BIN" ]; then
    echo "Kernel binary not found at: $KERNEL_BIN"
    echo "Build the kernel first: cargo build"
    exit 1
fi

echo "╔═══════════════════════════════════════╗"
echo "║     KontsnorOS — QEMU Launcher        ║"
echo "╠═══════════════════════════════════════╣"
echo "║  Build:  $BUILD_TYPE                  ║"
echo "║  Kernel: $KERNEL_BIN"
echo "║  Serial: stdio                        ║"
echo "╚═══════════════════════════════════════╝"
echo ""

echo "Building bootable disk image..."
bootloader_linker build "$KERNEL_BIN" -o "$PROJECT_DIR" -s

DISK_IMG="$PROJECT_DIR/disk.img"
if [ ! -f "$DISK_IMG" ]; then
    echo "Creating 10MB blank persistent hard drive image..."
    dd if=/dev/zero of="$DISK_IMG" bs=1M count=10 2>/dev/null
fi

qemu-system-x86_64 \
    -drive format=raw,file="$PROJECT_DIR/bios.img" \
    -drive format=raw,file="$DISK_IMG",index=1,media=disk \
    -serial stdio \
    -display none \
    -m 256M \
    -cpu qemu64,+fsgsbase \
    -no-reboot \
    -no-shutdown \
    $GDB_FLAG \
    "$@"
