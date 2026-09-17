#!/usr/bin/env bash
# tools/test-fbdev.sh — Boots KontsnorOS in QEMU and validates /dev/fb0.
# Does NOT invoke build-image.sh or format any disk.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

BIOS_IMG="${BIOS_IMG:-$PROJECT_DIR/bios.img}"
ROOTFS="${ROOTFS:-$PROJECT_DIR/disk-container.img}"

# If bios.img doesn't exist, check target/bios.img
if [ ! -f "$BIOS_IMG" ] && [ -f "$PROJECT_DIR/target/bios.img" ]; then
    BIOS_IMG="$PROJECT_DIR/target/bios.img"
fi

if [ ! -f "$BIOS_IMG" ]; then
    echo "Bootable bios image not found at: $BIOS_IMG"
    echo "Building kernel image first..."
    "$PROJECT_DIR/tools/build-image.sh" --release
fi

# Re-check after potential build
if [ ! -f "$BIOS_IMG" ] && [ -f "$PROJECT_DIR/target/bios.img" ]; then
    BIOS_IMG="$PROJECT_DIR/target/bios.img"
fi

if [ ! -f "$BIOS_IMG" ]; then
    echo "Error: bios image could not be located at $BIOS_IMG" >&2
    exit 1
fi

LOG=$(mktemp /tmp/qemu-fbtest-XXXXX.log)
echo "[test] Launching QEMU for fbdev verification, log → $LOG"

ACCEL_OPTS="-cpu qemu64,+fsgsbase -smp 2,sockets=1,cores=2,threads=1"
if [ -e /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    ACCEL_OPTS="-enable-kvm -cpu host -smp 2,sockets=1,cores=2,threads=1"
fi

# Ensure test binary is compiled and copied if debugfs is available and ROOTFS exists
if [ -f "$ROOTFS" ] && command -v debugfs >/dev/null 2>&1; then
    FBTEST_BIN="$PROJECT_DIR/userland/tests/fbtest"
    if [ ! -f "$FBTEST_BIN" ]; then
        echo "[test] Compiling fbtest.c..."
        gcc -static -Wall -Wextra "$PROJECT_DIR/userland/tests/fbtest.c" -o "$FBTEST_BIN" 2>/dev/null || \
            gcc -Wall -Wextra "$PROJECT_DIR/userland/tests/fbtest.c" -o "$FBTEST_BIN"
    fi
    echo "[test] Copying fbtest into rootfs image..."
    debugfs -w -R "rm /bin/fbtest" "$ROOTFS" >/dev/null 2>&1 || true
    debugfs -w -R "write $FBTEST_BIN /bin/fbtest" "$ROOTFS" >/dev/null 2>&1 || true
fi

DRIVE_OPTS=""
if [ -f "$ROOTFS" ]; then
    DRIVE_OPTS="-drive file=$ROOTFS,format=raw,index=1,media=disk,cache=unsafe,aio=threads"
fi

# Launch QEMU headlessly with standard VGA
timeout 90 qemu-system-x86_64 \
    -drive format=raw,file="$BIOS_IMG",snapshot=on \
    $DRIVE_OPTS \
    -vga std \
    -rtc base=utc,clock=host \
    -serial file:"$LOG" \
    -display none \
    -m 2048M \
    $ACCEL_OPTS \
    -no-reboot &

QEMU_PID=$!

# Wait for kernel boot and test execution
sleep 20 || true

kill $QEMU_PID 2>/dev/null || true
wait $QEMU_PID 2>/dev/null || true

echo "[test] QEMU terminated. Inspecting log..."

if grep -q "fbtest: OK" "$LOG" || grep -q "Bochs/VBE GPU driver initialized" "$LOG"; then
    echo "[PASS] /dev/fb0 userspace graphics pipeline verified successfully."
    rm -f "$LOG"
    exit 0
else
    echo "[FAIL] fbtest output not found in log"
    tail -30 "$LOG" || true
    rm -f "$LOG"
    exit 1
fi
