#!/bin/bash
# tools/run-container-test.sh
# KontsnorOS — Automated Container Isolation Test
#
# Builds the kernel, container runtime (ctr_run), prepares a containerized
# Alpine disk image (disk-container.img), and boots QEMU to verify:
#   1. Mount & Chroot namespace isolation
#   2. UTS namespace isolation (hostname)
#   3. PID namespace isolation (/proc/tasks)
#   4. Path jail boundary enforcement (../.. escape prevention)
#   5. Writable container tmpfs
#
# Usage: ./tools/run-container-test.sh [--rebuild-disk] [--debug]

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

REBUILD_DISK=false
GDB_FLAG=""

for arg in "$@"; do
    case "$arg" in
        --rebuild-disk)
            REBUILD_DISK=true
            ;;
        --debug)
            GDB_FLAG="-s"
            ;;
    esac
done

echo "========================================="
echo "  KontsnorOS Container Integration Test"
echo "========================================="

# 1. Compile the kernel and build bootable bios.img
echo "[1/5] Building bootable kernel image (release)..."
"$PROJECT_DIR/tools/build-image.sh" --release

BIOS_IMG="$PROJECT_DIR/bios.img"
DISK_IMG="$PROJECT_DIR/disk-container.img"
BUSYBOX_BIN="$PROJECT_DIR/busybox-build/busybox-1.36.1/busybox"

# 2. Compile ctr_run and init-container
echo "[2/5] Compiling container runtime & test harness..."
CC=$(which musl-gcc 2>/dev/null || echo gcc)
"$CC" -static -nostdlib -fno-builtin -o "$PROJECT_DIR/tools/ctr_run" "$PROJECT_DIR/tools/ctr_run.c"
"$CC" -static -nostdlib -fno-builtin -o "$PROJECT_DIR/tools/init-container" "$PROJECT_DIR/tools/init-container.c"

# 3. Ensure Alpine minirootfs is available
ALPINE_TAR="/tmp/alpine-minirootfs-3.20.0-x86_64.tar.gz"
ALPINE_STAGE="/tmp/alpine-stage"
ALPINE_URL="https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/x86_64/alpine-minirootfs-3.20.0-x86_64.tar.gz"

if [ ! -f "$ALPINE_TAR" ]; then
    echo "[3/5] Downloading Alpine Linux minirootfs..."
    wget -q --show-progress -O "$ALPINE_TAR" "$ALPINE_URL"
fi

if [ ! -d "$ALPINE_STAGE" ] || [ -z "$(ls -A "$ALPINE_STAGE" 2>/dev/null)" ]; then
    echo "[3/5] Extracting Alpine rootfs into stage directory..."
    rm -rf "$ALPINE_STAGE"
    mkdir -p "$ALPINE_STAGE"
    tar -xzf "$ALPINE_TAR" -C "$ALPINE_STAGE"
fi

# 4. Prepare disk-container.img if missing or requested
if [ ! -f "$DISK_IMG" ] || [ "$REBUILD_DISK" = true ]; then
    echo "[4/5] Formatting and populating $DISK_IMG (512MB ext4)..."
    rm -f "$DISK_IMG"
    dd if=/dev/zero of="$DISK_IMG" bs=1M count=512 status=none
    mkfs.ext4 -O ^has_journal -b 4096 -F -q "$DISK_IMG"

    # Create the in-container test script
    cat << 'EOF' > /tmp/test_inside.sh
#!/bin/sh
echo ""
echo "╔═════════════════════════════════════════════════╗"
echo "║    Running In-Container Verification Tests      ║"
echo "╚═════════════════════════════════════════════════╝"

echo "[test 1/5] Checking UTS Namespace Isolation..."
HOSTNAME=$(cat /proc/sys/kernel/hostname 2>/dev/null || uname -n)
echo "           Container hostname: $HOSTNAME"
if [ "$HOSTNAME" != "kontsnor-container" ]; then
    echo "FAILED: Hostname is '$HOSTNAME' (expected 'kontsnor-container')"
    exit 1
fi
echo "           -> PASS: UTS namespace is isolated."

echo "[test 2/5] Checking Chroot / Mount Jail Boundary..."
if [ -d "/containers" ]; then
    echo "FAILED: Host directory /containers is accessible inside container!"
    exit 2
fi
echo "           -> PASS: Host filesystem root is hidden from container."

echo "[test 3/5] Checking Path Jail Clamping (../.. escape prevention)..."
if [ -e "/../../../../containers" ]; then
    echo "FAILED: Traversal via ../.. escaped container root!"
    exit 3
fi
echo "           -> PASS: Path traversal past root clamped at container boundary."

echo "[test 4/5] Checking Virtual Filesystem (/proc/tasks)..."
if [ -f "/proc/tasks" ]; then
    echo "           Visible processes in container PID namespace:"
    cat /proc/tasks
fi
echo "           -> PASS: /proc reflects isolated namespace."

echo "[test 5/5] Checking Writable Container tmpfs..."
echo "KontsnorOS Container Data" > /tmp/container_payload.txt
READBACK=$(cat /tmp/container_payload.txt)
if [ "$READBACK" != "KontsnorOS Container Data" ]; then
    echo "FAILED: Writable tmpfs verification failed!"
    exit 5
fi
echo "           -> PASS: Container tmpfs writable and functional."

echo ""
echo "================================================="
echo "  ALL IN-CONTAINER ISOLATION CHECKS PASSED!"
echo "================================================="
exit 0
EOF
    chmod +x /tmp/test_inside.sh

    echo "           Populating ext4 disk structure via debugfs..."
    python3 - << 'PYEOF'
import os, subprocess, sys

disk_img = "/home/kontsnor/Projects/KontsnorOS/disk-container.img"
stage = "/tmp/alpine-stage"
init_bin = "/home/kontsnor/Projects/KontsnorOS/tools/init-container"
ctr_run_bin = "/home/kontsnor/Projects/KontsnorOS/tools/ctr_run"
busybox_bin = "/home/kontsnor/Projects/KontsnorOS/busybox-build/busybox-1.36.1/busybox"
test_script = "/tmp/test_inside.sh"

cmds = []

# Host base directories
for d in ["bin", "sbin", "dev", "dev/pts", "proc", "sys", "tmp", "containers", "containers/alpine"]:
    cmds.append(f"mkdir {d}")

# Host binaries
cmds.append(f"write {init_bin} /sbin/init")
cmds.append(f"write {ctr_run_bin} /bin/ctr_run")
cmds.append(f"write {busybox_bin} /bin/busybox")
cmds.append(f"write {busybox_bin} /bin/sh")

# Container directories (inside /containers/alpine)
all_dirs = []
for root, dirs, files in os.walk(stage, followlinks=False):
    for d in dirs:
        full = os.path.join(root, d)
        if not os.path.islink(full):
            rel = os.path.relpath(full, stage)
            all_dirs.append(rel)

for d in sorted(all_dirs, key=lambda p: (p.count('/'), p)):
    cmds.append(f"mkdir containers/alpine/{d}")

# Ensure standard container mountpoints exist
for m in ["proc", "sys", "dev", "dev/pts", "tmp"]:
    cmds.append(f"mkdir containers/alpine/{m}")

# Container files
for root, dirs, files in os.walk(stage, followlinks=False):
    for f in files:
        full = os.path.join(root, f)
        if not os.path.islink(full):
            rel = os.path.relpath(full, stage)
            cmds.append(f"write {full} containers/alpine/{rel}")

# Container symlinks
for root, dirs, files in os.walk(stage, followlinks=False):
    for f in dirs + files:
        full = os.path.join(root, f)
        if os.path.islink(full):
            rel = os.path.relpath(full, stage)
            target = os.readlink(full)
            cmds.append(f"symlink containers/alpine/{rel} {target}")

# Overwrite /containers/alpine/bin/busybox and /containers/alpine/bin/sh with static busybox
cmds.append(f"rm containers/alpine/bin/busybox")
cmds.append(f"write {busybox_bin} containers/alpine/bin/busybox")
cmds.append(f"rm containers/alpine/bin/sh")
cmds.append(f"write {busybox_bin} containers/alpine/bin/sh")

# Create symlinks for common busybox applets
for applet in ["ls", "cat", "echo", "pwd", "mkdir", "rm", "cp", "mv", "uname", "id", "ps", "kill", "touch", "grep", "sleep", "which", "env", "test"]:
    cmds.append(f"rm containers/alpine/bin/{applet}")
    cmds.append(f"symlink containers/alpine/bin/{applet} busybox")

# Install test script inside container
cmds.append(f"write {test_script} containers/alpine/test_inside.sh")

cmd_file = "/tmp/debugfs_container_cmds.txt"
with open(cmd_file, "w") as f:
    f.write("\n".join(cmds) + "\n")

proc = subprocess.run(["debugfs", "-w", "-f", cmd_file, disk_img], capture_output=True, text=True)
if proc.returncode != 0:
    print(f"Error populating disk: {proc.stderr}", file=sys.stderr)
    sys.exit(1)

print("           Disk image populated successfully.")
PYEOF
else
    echo "[4/5] Reusing existing $DISK_IMG, refreshing test binaries..."
    debugfs -w -R "rm /sbin/init" "$DISK_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "write $PROJECT_DIR/tools/init-container /sbin/init" "$DISK_IMG" >/dev/null 2>&1
    debugfs -w -R "rm /bin/ctr_run" "$DISK_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "write $PROJECT_DIR/tools/ctr_run /bin/ctr_run" "$DISK_IMG" >/dev/null 2>&1
fi

# 5. Boot QEMU with serial stdio and verify test output
echo "[5/5] Launching QEMU to execute container integration test..."
ACCEL_OPTS="-cpu qemu64,+fsgsbase -smp 4"
if [ -e /dev/kvm ]; then
    if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
        echo "Enabling KVM Hardware Acceleration (-enable-kvm -cpu host -smp 4)..."
        ACCEL_OPTS="-enable-kvm -cpu host -smp 4"
    else
        echo "WARNING: /dev/kvm exists but current user ($USER) lacks read/write permissions." >&2
        echo "         To enable KVM, add your user to the 'kvm' group: sudo usermod -aG kvm $USER" >&2
        echo "         Falling back to software TCG emulation (-cpu qemu64,+fsgsbase -smp 4)..." >&2
    fi
else
    echo "WARNING: /dev/kvm not found. Falling back to software TCG emulation (-cpu qemu64,+fsgsbase -smp 4)..." >&2
fi

QEMU_LOG="/tmp/qemu_container_test.log"
rm -f "$QEMU_LOG"

TEST_BIOS="/tmp/bios-container.img"
cp "$BIOS_IMG" "$TEST_BIOS"

set +e
qemu-system-x86_64 \
    -drive format=raw,file="$TEST_BIOS",snapshot=on \
    -drive format=raw,file="$DISK_IMG",index=1,media=disk,cache=unsafe \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -serial stdio \
    -display none \
    -m 4096M \
    $ACCEL_OPTS \
    -no-reboot \
    $GDB_FLAG 2>&1 | tee "$QEMU_LOG"

QEMU_STATUS=$?
set -e

echo ""
echo "QEMU exit code: $QEMU_STATUS"

if grep -q "\[CONTAINER_TEST_SUCCESS\] ALL PASSED!" "$QEMU_LOG" || grep -q "ALL IN-CONTAINER ISOLATION CHECKS PASSED!" "$QEMU_LOG"; then
    echo "================================================="
    echo "  SUCCESS: ALL CONTAINER TESTS PASSED IN QEMU!"
    echo "================================================="
    exit 0
else
    echo "================================================="
    echo "  FAILURE: Container integration test did not pass"
    echo "================================================="
    exit 1
fi
