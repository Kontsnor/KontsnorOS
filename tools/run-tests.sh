#!/bin/bash
# KontsnorOS — Run in-kernel test suite in QEMU
#
# Builds the kernel in test mode, packages it, runs QEMU with the
# isa-debug-exit device, and exits with 0 on success (33) and 1 on failure.

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

echo "Building kernel in test mode..."
cargo rustc --manifest-path "$PROJECT_DIR/kernel/Cargo.toml" --target x86_64-unknown-none --features test --release -- -Zpanic_abort_tests --test

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
bootloader_linker build "$STRIPPED_DIR/kontsnor-kernel" -o "$PROJECT_DIR/target" -s

DISK_IMG="$PROJECT_DIR/disk.img"
if [ ! -f "$DISK_IMG" ]; then
    echo "Creating 6GB blank persistent hard drive image for test disk..."
    dd if=/dev/zero of="$DISK_IMG" bs=1M count=6144 2>/dev/null
fi

ACCEL_OPTS="-cpu qemu64,+fsgsbase -smp 4,sockets=1,cores=4,threads=1"
if [ -e /dev/kvm ]; then
    if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
        echo "Enabling KVM Hardware Acceleration (-enable-kvm -cpu host -smp 4,sockets=1,cores=4,threads=1)..."
        ACCEL_OPTS="-enable-kvm -cpu host -smp 4,sockets=1,cores=4,threads=1"
    else
        echo "WARNING: /dev/kvm exists but current user ($USER) lacks read/write permissions." >&2
        echo "         To enable KVM, add your user to the 'kvm' group: sudo usermod -aG kvm $USER" >&2
        echo "         Falling back to software TCG emulation (-cpu qemu64,+fsgsbase -smp 4,sockets=1,cores=4,threads=1)..." >&2
    fi
else
    echo "WARNING: /dev/kvm not found. Falling back to software TCG emulation (-cpu qemu64,+fsgsbase -smp 4,sockets=1,cores=4,threads=1)..." >&2
fi

USE_NVME=false
for arg in "$@"; do
    case "$arg" in
        --nvme)
            USE_NVME=true
            ;;
    esac
done

STORAGE_OPTS="-drive file=$DISK_IMG,format=raw,index=1,media=disk,cache=unsafe,aio=threads,discard=unmap,detect-zeroes=unmap"
if [ "$USE_NVME" = true ]; then
    echo "Running test suite with NVMe PCIe device attached..."
    STORAGE_OPTS="-drive file=$DISK_IMG,if=none,id=nvm,format=raw,cache=unsafe,aio=threads,discard=unmap,detect-zeroes=unmap -device nvme,serial=kontsnor-nvme0,drive=nvm"
fi

echo "Starting QEMU in test mode..."
# Disable "exit on error" temporarily so we can capture the exit status from QEMU
set +e
qemu-system-x86_64 \
    -drive format=raw,file="$PROJECT_DIR/target/bios.img" \
    $STORAGE_OPTS \
    -rtc base=utc,clock=host \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -serial stdio \
    -display none \
    -m 4096M \
    $ACCEL_OPTS \
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
