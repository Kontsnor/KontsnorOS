#!/bin/bash
# KontsnorOS — Run kernel in QEMU with Ubuntu rootfs
#
# Usage: ./tools/run-ubuntu-qemu.sh [--release] [--debug]
#
# Options:
#   --release    Use the release build
#   --debug      Enable GDB debugging (port 1234)
#   --nvme       Attach disk-ubuntu.img as primary NVMe storage device
#   --with-nvme  Attach an additional NVMe drive alongside the ATA drive

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Parse arguments
BUILD_TYPE="release"
GDB_FLAG=""
USE_NVME=false
WITH_NVME=false
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
        --nvme)
            USE_NVME=true
            ;;
        --with-nvme)
            WITH_NVME=true
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
STORAGE_OPTS="-drive file=$DISK_IMG,format=raw,index=1,media=disk,cache=unsafe,aio=threads,discard=unmap,detect-zeroes=unmap"

if [ "$USE_NVME" = true ]; then
    echo "Configuring primary Ubuntu storage via NVMe PCIe Controller..."
    STORAGE_OPTS="-drive file=$DISK_IMG,if=none,id=nvm,format=raw,cache=unsafe,aio=threads,discard=unmap,detect-zeroes=unmap -device nvme,serial=kontsnor-nvme0,drive=nvm"
elif [ "$WITH_NVME" = true ]; then
    NVME_IMG="$PROJECT_DIR/nvme.img"
    if [ ! -f "$NVME_IMG" ]; then
        echo "Creating 2GB NVMe auxiliary drive image..."
        dd if=/dev/zero of="$NVME_IMG" bs=1M count=2048 2>/dev/null
    fi
    echo "Configuring primary ATA drive with auxiliary NVMe device..."
    STORAGE_OPTS="-drive file=$DISK_IMG,format=raw,index=1,media=disk,cache=unsafe,aio=threads,discard=unmap,detect-zeroes=unmap -drive file=$NVME_IMG,if=none,id=nvm,format=raw,cache=unsafe,aio=threads -device nvme,serial=kontsnor-nvme0,drive=nvm"
fi

trap 'stty sane 2>/dev/null || true' EXIT INT TERM

qemu-system-x86_64 \
    -drive format=raw,file="$BIOS_IMG" \
    $STORAGE_OPTS \
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
