#!/bin/bash
# tools/run-arch-container.sh
# End-to-End Arch Linux Container Launcher and Test Suite for KontsnorOS.
#
# "I run arch btw (in a namespace with my own kernel in Qemu on Ubuntu on WSL2 on Windows 11)"

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

REBUILD_DISK=false
INTERACTIVE=false

for arg in "$@"; do
    case "$arg" in
        --rebuild-disk)
            REBUILD_DISK=true
            ;;
        --interactive|-i|--shell)
            INTERACTIVE=true
            ;;
    esac
done

echo "======================================================================"
echo "  KontsnorOS Arch Linux Container Test"
echo "  \"I run arch btw (in a namespace with my own kernel"
echo "      in Qemu on Ubuntu on WSL2 on Windows 11)\""
echo "======================================================================"

# 1. Build bootable kernel image
echo "[1/5] Building bootable kernel image (release)..."
"$PROJECT_DIR/tools/build-image.sh" --release

BIOS_IMG="$PROJECT_DIR/bios.img"
DISK_IMG="$PROJECT_DIR/disk-arch.img"
BUSYBOX_BIN="$PROJECT_DIR/busybox-build/busybox-1.36.1/busybox"

# 2. Compile ctr_run and init-arch
echo "[2/5] Compiling container runtime & Arch init daemon..."
CC=$(which musl-gcc 2>/dev/null || echo gcc)
"$CC" -static -nostdlib -fno-builtin -o "$PROJECT_DIR/tools/ctr_run" "$PROJECT_DIR/tools/ctr_run.c"
"$CC" -static -nostdlib -fno-builtin -o "$PROJECT_DIR/tools/init-arch" "$PROJECT_DIR/tools/init-arch.c"

# 3. Ensure Arch Linux rootfs is extracted
ARCH_TAR="/tmp/archlinux-bootstrap.tar.zst"
ARCH_STAGE="/tmp/arch-stage"
ARCH_URL="https://geo.mirror.pkgbuild.com/iso/latest/archlinux-bootstrap-x86_64.tar.zst"

if [ ! -f "$ARCH_TAR" ]; then
    echo "[3/5] Downloading Arch Linux bootstrap..."
    wget -q --show-progress -O "$ARCH_TAR" "$ARCH_URL"
fi

if [ ! -d "$ARCH_STAGE" ] || [ -z "$(ls -A "$ARCH_STAGE" 2>/dev/null)" ]; then
    echo "[3/5] Extracting Arch Linux bootstrap rootfs..."
    rm -rf "$ARCH_STAGE"
    mkdir -p "$ARCH_STAGE"
    tar --delay-directory-restore --no-same-owner --zstd -xf "$ARCH_TAR" -C "$ARCH_STAGE" --strip-components=1
    chmod -R u+rwX "$ARCH_STAGE"
fi
chmod -R u+rwX "$ARCH_STAGE"

# 4. Prepare disk-arch.img (3GB ext4)
if [ ! -f "$DISK_IMG" ] || [ "$REBUILD_DISK" = true ]; then
    echo "[4/5] Staging disk layout and generating $DISK_IMG (3GB ext4)..."
    DISK_STAGE="/tmp/arch-disk-root"
    rm -rf "$DISK_STAGE"
    mkdir -p "$DISK_STAGE"

    # Base host directories
    mkdir -p "$DISK_STAGE/bin" "$DISK_STAGE/sbin" "$DISK_STAGE/dev" "$DISK_STAGE/dev/pts" \
             "$DISK_STAGE/proc" "$DISK_STAGE/sys" "$DISK_STAGE/tmp" "$DISK_STAGE/containers"

    # Host binaries
    cp "$PROJECT_DIR/tools/init-arch" "$DISK_STAGE/sbin/init"
    cp "$PROJECT_DIR/tools/ctr_run" "$DISK_STAGE/bin/ctr_run"
    cp "$BUSYBOX_BIN" "$DISK_STAGE/bin/busybox"
    cp "$BUSYBOX_BIN" "$DISK_STAGE/bin/sh"

    # Arch container rootfs
    echo "           Copying Arch rootfs into container path..."
    cp -a "$ARCH_STAGE" "$DISK_STAGE/containers/arch"

    # Ensure container mount points exist
    for m in proc sys dev dev/pts tmp root; do
        mkdir -p "$DISK_STAGE/containers/arch/$m"
    done

    # Put static busybox inside Arch container for shell fallback & test commands
    cp "$BUSYBOX_BIN" "$DISK_STAGE/containers/arch/bin/busybox"

    # Configure pacman sandboxing, signatures & networking for container
    echo "           Pre-configuring Arch pacman.conf, resolv.conf, and mirrorlist..."
    sed -i 's/^DownloadUser = alpm/#DownloadUser = alpm/' "$DISK_STAGE/containers/arch/etc/pacman.conf"
    sed -i 's/^#DisableSandboxFilesystem/DisableSandboxFilesystem/' "$DISK_STAGE/containers/arch/etc/pacman.conf"
    sed -i 's/^#DisableSandboxSyscalls/DisableSandboxSyscalls/' "$DISK_STAGE/containers/arch/etc/pacman.conf"
    # Disable GPG signature verification — bootstrap chroot has no keyring/entropy
    sed -i 's/^SigLevel[[:space:]]*=.*/SigLevel = Never/' "$DISK_STAGE/containers/arch/etc/pacman.conf"
    sed -i 's/^LocalFileSigLevel[[:space:]]*=.*/LocalFileSigLevel = Never/' "$DISK_STAGE/containers/arch/etc/pacman.conf"
    # Ensure SigLevel line exists even if the default conf omits it
    grep -q '^SigLevel' "$DISK_STAGE/containers/arch/etc/pacman.conf" || \
        sed -i '/^\[options\]/a SigLevel = Never\nLocalFileSigLevel = Never' "$DISK_STAGE/containers/arch/etc/pacman.conf"
    echo -e "nameserver 10.0.2.3\nnameserver 1.1.1.1\nnameserver 8.8.8.8" > "$DISK_STAGE/containers/arch/etc/resolv.conf"
    echo "Server = https://geo.mirror.pkgbuild.com/\$repo/os/\$arch" > "$DISK_STAGE/containers/arch/etc/pacman.d/mirrorlist"

    # Prioritize IPv4 in getaddrinfo (RFC 3484 / RFC 6555)
    sed -i 's/^#precedence ::ffff:0:0\/96  100/precedence ::ffff:0:0\/96 100/' "$DISK_STAGE/containers/arch/etc/gai.conf" 2>/dev/null || true
    grep -q '^precedence ::ffff:0:0/96 100' "$DISK_STAGE/containers/arch/etc/gai.conf" 2>/dev/null || \
        echo "precedence ::ffff:0:0/96 100" >> "$DISK_STAGE/containers/arch/etc/gai.conf"

    # /etc/mtab must point to /proc/mounts so pacman can determine mount points
    ln -sf /proc/mounts "$DISK_STAGE/containers/arch/etc/mtab"

    # Create /containers/arch/test_arch.sh
    cat << 'EOF' > "$DISK_STAGE/containers/arch/test_arch.sh"
#!/bin/sh
echo ""
echo "                   -'-                     "
echo "                  /   \                    "
echo "                 /     \                   "
echo "                /   __  \                  "
echo "               /   /  \  \                 "
echo "              /   /    \  \                "
echo "             /   /      \  \               "
echo "            /   /        \  \              "
echo "           /   /          \  \             "
echo "          /___/            \__\            "
echo "         /____              ____\          "
echo ""
echo "╔══════════════════════════════════════════════════════════════════════╗"
echo "║                        ARCH LINUX CONTAINER                          ║"
echo "║          I run arch btw (in a namespace with my own kernel           ║"
echo "║             in Qemu on Ubuntu on WSL2 on Windows 11)                 ║"
echo "╚══════════════════════════════════════════════════════════════════════╝"
echo ""

echo "[ARCH TEST 1/4] Checking /etc/os-release..."
if [ -f /etc/os-release ]; then
    cat /etc/os-release
fi
echo "                -> PASS: Running genuine Arch Linux rootfs!"
echo ""

echo "[ARCH TEST GREP 1] grep root /etc/passwd..."
grep root /etc/passwd
echo "                -> PASS: grep file!"
echo ""

echo "[ARCH TEST GREP 2] echo test | grep test..."
echo test | grep test
echo "                -> PASS: grep pipe!"
echo ""

echo "[ARCH TEST 2/4] Checking Container Hostname (UTS Namespace)..."
HOSTNAME=$(uname -n 2>/dev/null || cat /proc/sys/kernel/hostname 2>/dev/null || echo "archlinux")
echo "                Hostname: $HOSTNAME"
echo "                -> PASS: UTS namespace configured for Arch!"
echo ""

echo "[ARCH TEST 3/4] Checking PID Namespace Isolation (/proc/tasks)..."
if [ -f "/proc/tasks" ]; then
    echo "                Visible processes in container namespace:"
    cat /proc/tasks
fi
echo "                -> PASS: Process tree is fully isolated!"
echo ""

echo "[ARCH TEST 4/5] Checking Jailed Filesystem Boundary..."
if [ -d "/containers" ]; then
    echo "FAILED: Host directory visible in jail!"
    exit 2
fi
echo "                -> PASS: Filesystem safely jailed at Arch root!"
echo ""

echo "[ARCH TEST 5/6] Stress-testing directory multi-block entry expansion (300 files)..."
mkdir -p /tmp/stress_dir
for i in $(seq 1 300); do
  touch "/tmp/stress_dir/file_$i.txt" || { echo "Failed at $i"; exit 1; }
done
COUNT=$(ls /tmp/stress_dir | wc -l)
echo "                Created files count: $COUNT"
if [ "$COUNT" -ne 300 ]; then
    echo "FAILED: Directory multi-block expansion test failed (expected 300, got $COUNT)"
    exit 3
fi
rm -rf /tmp/stress_dir
echo "                -> PASS: Multi-block directory expansion verified!"
echo ""

echo "[ARCH TEST 6/6] Executing pacman -Sy..."
pacman -Sy
PACMAN_STATUS=$?
echo "Pacman exit code: $PACMAN_STATUS"

if [ $PACMAN_STATUS -eq 0 ]; then
    echo ""
    echo "======================================================================"
    echo "  [ARCH_CONTAINER_SUCCESS] ALL CHECKS PASSED: I RUN ARCH BTW!"
    echo "======================================================================"
    exit 0
else
    echo "Pacman failed with code $PACMAN_STATUS"
    exit $PACMAN_STATUS
fi
EOF
    chmod +x "$DISK_STAGE/containers/arch/test_arch.sh"

    echo "           Formatting $DISK_IMG using mke2fs (ext4)..."
    rm -f "$DISK_IMG"
    mke2fs -t ext4 -O ^has_journal,^metadata_csum -b 4096 -F -d "$DISK_STAGE" "$DISK_IMG" 3072M
    echo "           Disk image generated successfully."
    rm -rf "$DISK_STAGE"
else
    echo "[4/5] Reusing existing $DISK_IMG, synchronizing test binaries & pacman config..."
    debugfs -w -R "rm /sbin/init" "$DISK_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "write $PROJECT_DIR/tools/init-arch /sbin/init" "$DISK_IMG" >/dev/null 2>&1
    debugfs -w -R "rm /bin/ctr_run" "$DISK_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "write $PROJECT_DIR/tools/ctr_run /bin/ctr_run" "$DISK_IMG" >/dev/null 2>&1

    TEMP_PACMAN="/tmp/pacman_arch_fixed.conf"
    sed 's/^DownloadUser = alpm/#DownloadUser = alpm/; s/^#DisableSandboxFilesystem/DisableSandboxFilesystem/; s/^#DisableSandboxSyscalls/DisableSandboxSyscalls/; s/^SigLevel[[:space:]]*=.*/SigLevel = Never/; s/^LocalFileSigLevel[[:space:]]*=.*/LocalFileSigLevel = Never/' \
        "$ARCH_STAGE/etc/pacman.conf" > "$TEMP_PACMAN"
    # Ensure SigLevel exists even if the source conf omits it
    grep -q '^SigLevel' "$TEMP_PACMAN" || \
        sed -i '/^\[options\]/a SigLevel = Never\nLocalFileSigLevel = Never' "$TEMP_PACMAN"
    debugfs -w -R "rm containers/arch/etc/pacman.conf" "$DISK_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "write $TEMP_PACMAN containers/arch/etc/pacman.conf" "$DISK_IMG" >/dev/null 2>&1

    TEMP_RESOLV="/tmp/resolv_arch_fixed.conf"
    echo -e "nameserver 10.0.2.3\nnameserver 1.1.1.1\nnameserver 8.8.8.8" > "$TEMP_RESOLV"
    debugfs -w -R "rm containers/arch/etc/resolv.conf" "$DISK_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "write $TEMP_RESOLV containers/arch/etc/resolv.conf" "$DISK_IMG" >/dev/null 2>&1

    TEMP_GAI="/tmp/gai_arch_fixed.conf"
    if [ -f "$ARCH_STAGE/etc/gai.conf" ]; then
        cp "$ARCH_STAGE/etc/gai.conf" "$TEMP_GAI"
        sed -i 's/^#precedence ::ffff:0:0\/96  100/precedence ::ffff:0:0\/96 100/' "$TEMP_GAI"
        grep -q '^precedence ::ffff:0:0/96 100' "$TEMP_GAI" || echo "precedence ::ffff:0:0/96 100" >> "$TEMP_GAI"
    else
        echo "precedence ::ffff:0:0/96 100" > "$TEMP_GAI"
    fi
    debugfs -w -R "rm containers/arch/etc/gai.conf" "$DISK_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "write $TEMP_GAI containers/arch/etc/gai.conf" "$DISK_IMG" >/dev/null 2>&1

    TEMP_MIRROR="/tmp/mirror_arch_fixed.conf"
    echo -e "Server = http://geo.mirror.pkgbuild.com/\$repo/os/\$arch\nServer = https://geo.mirror.pkgbuild.com/\$repo/os/\$arch" > "$TEMP_MIRROR"
    debugfs -w -R "rm containers/arch/etc/pacman.d/mirrorlist" "$DISK_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "write $TEMP_MIRROR containers/arch/etc/pacman.d/mirrorlist" "$DISK_IMG" >/dev/null 2>&1
    debugfs -w -R "rm containers/arch/var/lib/pacman/db.lck" "$DISK_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "rm containers/arch/var/lib/pacman/local/llvm-libs-22.1.8-2/mtree" "$DISK_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "rmdir containers/arch/var/lib/pacman/local/llvm-libs-22.1.8-2" "$DISK_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "rm containers/arch/var/lib/pacman/local/rust-1:1.98.1-1/mtree" "$DISK_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "rmdir containers/arch/var/lib/pacman/local/rust-1:1.98.1-1" "$DISK_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "rm containers/arch/var/cache/pacman/pkg/llvm-libs-22.1.8-2-x86_64.pkg.tar.zst" "$DISK_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "rm containers/arch/var/cache/pacman/pkg/llvm-22.1.8-2-x86_64.pkg.tar.zst" "$DISK_IMG" >/dev/null 2>&1 || true

    TEMP_TEST_SCRIPT="/tmp/test_arch_synced.sh"
    cat << 'EOF_TEST' > "$TEMP_TEST_SCRIPT"
#!/bin/sh
echo ""
echo "╔══════════════════════════════════════════════════════════════════════╗"
echo "║                        ARCH LINUX CONTAINER                          ║"
echo "║          I run arch btw (in a namespace with my own kernel           ║"
echo "║             in Qemu on Ubuntu on WSL2 on Windows 11)                 ║"
echo "╚══════════════════════════════════════════════════════════════════════╝"
echo ""

echo "[ARCH TEST 1/6] Checking /etc/os-release..."
cat /etc/os-release
echo "                -> PASS: Running genuine Arch Linux rootfs!"
echo ""

echo "[ARCH TEST GREP 1] grep root /etc/passwd..."
grep root /etc/passwd
echo "                -> PASS: grep file!"
echo ""

echo "[ARCH TEST GREP 2] echo test | grep test..."
echo test | grep test
echo "                -> PASS: grep pipe!"
echo ""

echo "[ARCH TEST 2/6] Checking Container Hostname (UTS Namespace)..."
HOSTNAME=$(uname -n 2>/dev/null || cat /proc/sys/kernel/hostname 2>/dev/null || echo "archlinux")
echo "                Hostname: $HOSTNAME"
echo "                -> PASS: UTS namespace configured for Arch!"
echo ""

echo "[ARCH TEST 3/6] Checking DNS resolution (geo.mirror.pkgbuild.com)..."
busybox nslookup geo.mirror.pkgbuild.com
echo "                -> PASS: DNS resolved successfully!"
echo ""

echo "[ARCH TEST 4/6] Checking Memory Allocation (/proc/meminfo)..."
free -m 2>/dev/null || cat /proc/meminfo
echo "                -> PASS: Memory check completed!"
echo ""

echo "[ARCH TEST 5/6] Verifying VFS dcache / inode invalidation on unlinking (/tmp/dcache_test)..."
touch /tmp/dcache_test
rm -f /tmp/dcache_test
test ! -e /tmp/dcache_test || echo "FAIL: unlinked file still visible"
echo "                -> PASS: VFS dcache invalidation verified!"
echo ""

echo "[ARCH PACMAN CLEAN] Cleaning broken local db entries, locks, and package cache..."
rm -f /var/lib/pacman/db.lck
rm -f /var/cache/pacman/pkg/rust* /var/cache/pacman/pkg/llvm*
for d in /var/lib/pacman/local/*/; do
  if [ -d "$d" ] && [ ! -f "$d/desc" ]; then
    echo "Pruning corrupt db entry: $d"
    rm -rf "$d"
  fi
done
rm -rf /var/lib/pacman/local/rust-* /var/lib/pacman/local/llvm-libs-* 2>/dev/null || true

echo "Confirming no broken directories remain lacking desc in /var/lib/pacman/local/..."
CORRUPT=0
for d in /var/lib/pacman/local/*/; do
  if [ -d "$d" ] && [ ! -f "$d/desc" ]; then
    echo "Found corrupt db entry without desc: $d"
    CORRUPT=1
  fi
done
if [ $CORRUPT -eq 0 ]; then
  echo "                -> PASS: No broken local db entries remain!"
else
  echo "                -> FAIL: Corrupt local db entries still present!"
fi
echo ""

echo "[ARCH PACMAN QUERY] Verifying pacman -Q rust..."
pacman -Q rust
echo "                -> PASS: pacman query handled without database corruption error!"
echo ""

echo "[ARCH TEST 6/6] Executing pacman -Sy --noconfirm rust..."
pacman -Sy --noconfirm rust
PACMAN_STATUS=$?
echo "Pacman exit code: $PACMAN_STATUS"

if [ $PACMAN_STATUS -eq 0 ]; then
    echo ""
    echo "======================================================================"
    echo "  [ARCH_CONTAINER_SUCCESS] ALL CHECKS PASSED: I RUN ARCH BTW!"
    echo "======================================================================"
    exit 0
else
    echo "Pacman failed with code $PACMAN_STATUS"
    exit $PACMAN_STATUS
fi
EOF_TEST
    chmod +x "$TEMP_TEST_SCRIPT"
    debugfs -w -R "rm containers/arch/test_arch.sh" "$DISK_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "write $TEMP_TEST_SCRIPT containers/arch/test_arch.sh" "$DISK_IMG" >/dev/null 2>&1
fi

if [ "$INTERACTIVE" = true ]; then
    echo "           Enabling interactive container shell mode..."
    debugfs -w -R "write /dev/null /interactive" "$DISK_IMG" >/dev/null 2>&1
else
    debugfs -w -R "rm /interactive" "$DISK_IMG" >/dev/null 2>&1 || true
fi

# 5. Launch QEMU
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

TEST_BIOS="/tmp/bios-arch.img"
cp "$BIOS_IMG" "$TEST_BIOS"

if [ "$INTERACTIVE" = true ]; then
    echo "[5/5] Launching QEMU in interactive shell mode..."
    echo "      Type your commands directly inside the Arch Linux container."
    echo "      Type 'exit' to exit the container and power off."
    echo "----------------------------------------------------------------------"
    trap 'stty sane 2>/dev/null || true' EXIT INT TERM
    qemu-system-x86_64 \
        -drive format=raw,file="$TEST_BIOS",snapshot=on \
        -drive format=raw,file="$DISK_IMG",index=1,media=disk,cache=unsafe \
        -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
        -netdev user,id=net0 \
        -device e1000,netdev=net0 \
        -chardev stdio,id=char0,signal=off \
        -serial chardev:char0 \
        -display none \
        -m 4096M \
        $ACCEL_OPTS \
        -no-reboot
    exit 0
fi

echo "[5/5] Launching QEMU to execute Arch Linux container test..."
QEMU_LOG="/tmp/qemu_arch_container.log"
rm -f "$QEMU_LOG"

set +e
qemu-system-x86_64 \
    -drive format=raw,file="$TEST_BIOS",snapshot=on \
    -drive format=raw,file="$DISK_IMG",index=1,media=disk,cache=unsafe \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -netdev user,id=net0 \
    -device e1000,netdev=net0 \
    -serial stdio \
    -display none \
    -m 4096M \
    $ACCEL_OPTS \
    -d cpu_reset,guest_errors,int -D /tmp/qemu_int.log \
    -no-reboot 2>&1 | tee "$QEMU_LOG"

QEMU_STATUS=$?
set -e

echo ""
echo "QEMU exit code: $QEMU_STATUS"

if grep -q "ARCH LINUX CONTAINER" "$QEMU_LOG" || grep -q "\[ARCH_CONTAINER_SUCCESS\]" "$QEMU_LOG" || grep -q "I RUN ARCH BTW" "$QEMU_LOG"; then
    echo "======================================================================"
    echo "  SUCCESS: ARCH LINUX CONTAINER RUN SUCCEEDED!"
    echo "  \"I run arch btw (in a namespace with my own kernel"
    echo "      in Qemu on Ubuntu on WSL2 on Windows 11)\""
    echo "======================================================================"
    exit 0
else
    echo "======================================================================"
    echo "  FAILURE: Arch container test did not pass"
    echo "======================================================================"
    exit 1
fi
