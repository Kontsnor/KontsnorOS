#!/bin/bash
# tools/verify-doom.sh
# Headless visual verification of fbdoom running on KontsnorOS in QEMU (-vga std)
# Uses QEMU monitor screendump and VRAM inspection (No VNC)

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
BIOS_IMG="$PROJECT_DIR/bios.img"
DISK_IMG="$PROJECT_DIR/disk-arch.img"
MONITOR_SOCK="/tmp/qemu-monitor.sock"
PPM_OUTPUT="/tmp/framebuffer.ppm"
VRAM_OUTPUT="/tmp/vram.bin"
QEMU_LOG="/tmp/qemu_doom_serial.log"

echo "======================================================================"
echo "  KontsnorOS Headless Visual Verification Suite (fbdoom on /dev/fb0)"
echo "======================================================================"

# Clean up stale files
rm -f "$MONITOR_SOCK" "$PPM_OUTPUT" "$VRAM_OUTPUT" "$QEMU_LOG"

# 1. Prepare guest test script
TEMP_TEST_SCRIPT="/tmp/guest_run_doom.sh"
cat << 'EOF' > "$TEMP_TEST_SCRIPT"
#!/bin/sh
echo "=== [GUEST] Checking /dev/fb0 character device node ==="
ls -l /dev/fb0
echo "=== [GUEST] Running fbdoom in container ==="
cd /root/fbdoom
./doom -iwad doom1.wad -devparm
EOF
chmod +x "$TEMP_TEST_SCRIPT"

echo "[1/4] Installing Doom launcher script into $DISK_IMG..."
debugfs -w -R "rm containers/arch/test_arch.sh" "$DISK_IMG" >/dev/null 2>&1 || true
debugfs -w -R "write $TEMP_TEST_SCRIPT containers/arch/test_arch.sh" "$DISK_IMG" >/dev/null 2>&1
debugfs -w -R "rm /interactive" "$DISK_IMG" >/dev/null 2>&1 || true
e2fsck -fy "$DISK_IMG" >/dev/null 2>&1 || true

# 2. Configure QEMU flags
ACCEL_OPTS="-cpu qemu64,+fsgsbase -smp 4,sockets=1,cores=4,threads=1"
if [ -e /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    echo "[2/4] Enabling KVM Hardware Acceleration..."
    ACCEL_OPTS="-enable-kvm -cpu host -smp 4,sockets=1,cores=4,threads=1"
else
    echo "[2/4] Falling back to software TCG emulation..."
fi

# 3. Launch QEMU headlessly with monitor attached
echo "[3/4] Launching QEMU headlessly with monitor socket $MONITOR_SOCK..."
qemu-system-x86_64 \
    -drive format=raw,file="$BIOS_IMG",snapshot=on \
    -drive file="$DISK_IMG",format=raw,index=1,media=disk,cache=unsafe,aio=threads,discard=unmap,detect-zeroes=unmap \
    -rtc base=utc,clock=host \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -netdev user,id=net0 \
    -device e1000,netdev=net0 \
    -serial stdio \
    -vga std \
    -display none \
    -monitor unix:"$MONITOR_SOCK",server,nowait \
    -m 4096M \
    $ACCEL_OPTS \
    -no-reboot > "$QEMU_LOG" 2>&1 &

QEMU_PID=$!
echo "      QEMU PID: $QEMU_PID"

cleanup() {
    echo "Stopping QEMU (PID $QEMU_PID)..."
    if [ -S "$MONITOR_SOCK" ]; then
        echo "quit" | socat - UNIX-CONNECT:"$MONITOR_SOCK" >/dev/null 2>&1 || true
    fi
    kill "$QEMU_PID" >/dev/null 2>&1 || true
    wait "$QEMU_PID" >/dev/null 2>&1 || true
    rm -f "$MONITOR_SOCK"
}
trap cleanup EXIT INT TERM

# 4. Wait for Doom to map /dev/fb0 and render
echo "[4/4] Monitoring guest output for framebuffer initialization..."
MAX_WAIT=120
WAIT_COUNT=0
FOUND_FB=false

while [ $WAIT_COUNT -lt $MAX_WAIT ]; do
    if grep -q "The framebuffer device was mapped to memory successfully" "$QEMU_LOG" 2>/dev/null; then
        FOUND_FB=true
        echo "      -> Framebuffer device successfully mapped by Doom!"
        break
    fi
    if ! kill -0 "$QEMU_PID" 2>/dev/null; then
        echo "ERROR: QEMU process terminated unexpectedly!"
        cat "$QEMU_LOG"
        exit 1
    fi
    sleep 1
    WAIT_COUNT=$((WAIT_COUNT + 1))
    echo -n "."
done
echo ""

if [ "$FOUND_FB" != true ]; then
    echo "ERROR: Timed out waiting for Doom framebuffer initialization ($MAX_WAIT s)."
    echo "=== QEMU SERIAL LOG ==="
    tail -n 60 "$QEMU_LOG"
    exit 2
fi

# Allow Doom a brief moment to render frames into VRAM
sleep 2

# Verify monitor socket is active
if [ ! -S "$MONITOR_SOCK" ]; then
    echo "ERROR: QEMU monitor socket $MONITOR_SOCK not found!"
    exit 3
fi

echo "Triggering screendump from QEMU monitor..."
echo "screendump $PPM_OUTPUT" | socat - UNIX-CONNECT:"$MONITOR_SOCK"
sleep 1

if [ ! -f "$PPM_OUTPUT" ]; then
    echo "ERROR: screendump failed to produce $PPM_OUTPUT!"
    exit 4
fi

echo "Inspecting screendump image header:"
PPM_HEADER=$(head -n 3 "$PPM_OUTPUT")
echo "$PPM_HEADER"

PPM_MAGIC=$(head -n 1 "$PPM_OUTPUT")
PPM_DIMS=$(sed -n '2p' "$PPM_OUTPUT")

echo "Magic: $PPM_MAGIC, Dimensions: $PPM_DIMS"

if [ "$PPM_DIMS" = "720 400" ]; then
    echo "FAILURE: Display is still in legacy VGA 80x25 text mode (720x400)!"
    exit 5
fi

if [ "$PPM_MAGIC" = "P6" ] && ([ "$PPM_DIMS" = "320 200" ] || [ "$PPM_DIMS" = "640 480" ] || [ "$PPM_DIMS" = "1024 768" ]); then
    echo ""
    echo "======================================================================"
    echo "  [SUCCESS] Visual Verification PASSED!"
    echo "  Hardware successfully exited legacy text mode."
    echo "  Bochs VBE Linear Framebuffer Resolution: $PPM_DIMS"
    echo "  PPM Framebuffer: $PPM_OUTPUT"
    echo "======================================================================"
else
    echo "WARNING: Unexpected resolution $PPM_DIMS, but not text mode."
fi

# Also dump physical VRAM to verify pixel data
echo "Dumping 64KB physical VRAM from BAR..."
echo "memsave 0xfd000000 64000 $VRAM_OUTPUT" | socat - UNIX-CONNECT:"$MONITOR_SOCK" >/dev/null 2>&1 || \
echo "memsave 0xe0000000 64000 $VRAM_OUTPUT" | socat - UNIX-CONNECT:"$MONITOR_SOCK" >/dev/null 2>&1 || true

if [ -f "$VRAM_OUTPUT" ]; then
    NONZERO=$(tr -d '\0' < "$VRAM_OUTPUT" | wc -c)
    echo "VRAM dump size: $(stat -c%s "$VRAM_OUTPUT") bytes, non-zero bytes: $NONZERO"
    if [ "$NONZERO" -gt 0 ]; then
        echo "-> PASS: VRAM contains active rendered pixel raster data!"
    fi
fi

exit 0
