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
    echo "Creating 6GB blank persistent hard drive image..."
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
rm -f /tmp/qmp-kontsnor.sock

trap 'stty sane 2>/dev/null || true' EXIT INT TERM

qemu-system-x86_64 \
    -drive format=raw,file="$BIOS_IMG" \
    -drive file="$DISK_IMG",format=raw,index=1,media=disk,cache=unsafe,aio=threads,discard=unmap,detect-zeroes=unmap \
    -rtc base=utc,clock=host \
    -chardev stdio,id=char0,signal=off \
    -serial chardev:char0 \
    -display none \
    -m 4096M \
    -netdev user,id=net0 \
    -device e1000,netdev=net0 \
    -qmp unix:/tmp/qmp-kontsnor.sock,server,nowait \
    $ACCEL_OPTS \
    -no-reboot \
    -no-shutdown \
    $GDB_FLAG \
    "${QEMU_ARGS[@]}"
