#!/bin/bash
# KontsnorOS — Run in-kernel test suite in QEMU
#
# Builds the kernel in test mode, packages it, runs QEMU with the
# isa-debug-exit device, and exits with 0 on success (33) and 1 on failure.

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

echo "Building kernel in test mode..."
cargo rustc --manifest-path "$PROJECT_DIR/kernel/Cargo.toml" --features test --release -- --test

echo "Building bootable test image..."
KERNEL_BIN="$PROJECT_DIR/target/x86_64-unknown-none/release/kontsnor-kernel"

if [ ! -f "$KERNEL_BIN" ]; then
    echo "Kernel binary not found at: $KERNEL_BIN"
    exit 1
fi

STRIPPED_DIR="$PROJECT_DIR/target/stripped"
mkdir -p "$STRIPPED_DIR"
cp "$KERNEL_BIN" "$STRIPPED_DIR/kontsnor-kernel"
strip "$STRIPPED_DIR/kontsnor-kernel"
bootloader_linker build "$STRIPPED_DIR/kontsnor-kernel" -o "$PROJECT_DIR" -s

echo "Starting QEMU in test mode..."
# Disable "exit on error" temporarily so we can capture the exit status from QEMU
set +e
qemu-system-x86_64 \
    -drive format=raw,file="$PROJECT_DIR/bios.img" \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -serial stdio \
    -display none \
    -m 256M \
    -smp 4 \
    -cpu qemu64,+fsgsbase \
    -no-reboot
QEMU_STATUS=$?
set -e

echo ""
echo "QEMU exit code: $QEMU_STATUS"

if [ "$QEMU_STATUS" -eq 33 ]; then
    echo "========================================="
    echo "  ALL TESTS PASSED SUCCESSFULLY!"
    echo "========================================="
    exit 0
elif [ "$QEMU_STATUS" -eq 35 ]; then
    echo "========================================="
    echo "  TEST SUITE FAILURE!"
    echo "========================================="
    exit 1
else
    echo "Unexpected QEMU exit code: $QEMU_STATUS"
    exit 1
fi
