#!/bin/bash
# Formats disk-ubuntu.img with ext2 and injects Ubuntu 24.04 Base rootfs and custom init
set -e

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DISK_IMG="$PROJECT_DIR/disk-ubuntu.img"
INIT_BIN="$PROJECT_DIR/tools/init-ubuntu"
INIT_SRC="$PROJECT_DIR/tools/init-ubuntu.c"

echo "=== KontsnorOS Ubuntu Disk Image Builder ==="

echo "[1/5] Compiling Ubuntu init binary..."
musl-gcc -static -nostdlib -o "$INIT_BIN" "$INIT_SRC"
chmod +x "$INIT_BIN"

UBUNTU_TAR="/tmp/ubuntu-base-24.04.4-base-amd64.tar.gz"
UBUNTU_STAGE="/tmp/ubuntu-stage"
UBUNTU_URL="https://cdimage.ubuntu.com/ubuntu-base/releases/24.04/release/ubuntu-base-24.04.4-base-amd64.tar.gz"

if [ ! -f "$UBUNTU_TAR" ]; then
    echo "[2/5] Downloading Ubuntu Base 24.04 LTS rootfs..."
    wget -q --show-progress -O "$UBUNTU_TAR" "$UBUNTU_URL"
fi

if [ ! -d "$UBUNTU_STAGE" ] || [ -z "$(ls -A "$UBUNTU_STAGE" 2>/dev/null)" ]; then
    echo "[3/5] Extracting Ubuntu Base rootfs..."
    rm -rf "$UBUNTU_STAGE"
    mkdir -p "$UBUNTU_STAGE"
    tar -xzf "$UBUNTU_TAR" -C "$UBUNTU_STAGE"
fi

echo "[4/5] Creating and formatting 6GB disk image: $DISK_IMG..."
dd if=/dev/zero of="$DISK_IMG" bs=1M count=6144 status=progress
mkfs.ext4 -b 4096 -F "$DISK_IMG"

echo "[5/5] Populating ext2 filesystem via debugfs..."
python3 - << PYEOF
import os, subprocess, sys

disk_img = "$DISK_IMG"
stage = "$UBUNTU_STAGE"
init_bin = "$INIT_BIN"

cmds = []

# 1. Directories (sorted so parents are created before children)
all_dirs = []
for root, dirs, files in os.walk(stage, followlinks=False):
    for d in dirs:
        full = os.path.join(root, d)
        if not os.path.islink(full):
            rel = os.path.relpath(full, stage)
            all_dirs.append(rel)

for d in sorted(all_dirs, key=lambda p: (p.count('/'), p)):
    cmds.append(f"mkdir {d}")

# Ensure standard mountpoints exist
for extra_dir in ["proc", "sys", "dev", "dev/pts", "tmp", "root", "home", "disk"]:
    cmds.append(f"mkdir {extra_dir}")

# 2. Regular files
for root, dirs, files in os.walk(stage, followlinks=False):
    for f in files:
        full = os.path.join(root, f)
        if not os.path.islink(full):
            rel = os.path.relpath(full, stage)
            cmds.append(f"write {full} {rel}")

# 3. Symlinks
for root, dirs, files in os.walk(stage, followlinks=False):
    for entry in dirs + files:
        full = os.path.join(root, entry)
        if os.path.islink(full):
            rel = os.path.relpath(full, stage)
            target = os.readlink(full)
            cmds.append(f"symlink {rel} {target}")

# 4. Install custom static init
cmds.append(f"rm /init")
cmds.append(f"write {init_bin} /init")
cmds.append(f"rm /usr/sbin/init")
cmds.append(f"write {init_bin} /usr/sbin/init")

# 5. Create a convenient issue / welcome banner in /etc/issue
with open("/tmp/kontsnor_ubuntu_issue", "w") as f:
    f.write("\nUbuntu 24.04 LTS running on KontsnorOS Kernel (x86_64)\n\n")
cmds.append("write /tmp/kontsnor_ubuntu_issue /etc/issue")

# 6. Configure DNS nameservers in /etc/resolv.conf
with open("/tmp/kontsnor_resolv_conf", "w") as f:
    f.write("nameserver 10.0.2.3\nnameserver 1.1.1.1\nnameserver 8.8.8.8\n")
cmds.append("rm /etc/resolv.conf")
cmds.append("write /tmp/kontsnor_resolv_conf /etc/resolv.conf")

cmd_str = "\n".join(cmds) + "\n"
proc = subprocess.run(["debugfs", "-w", disk_img], input=cmd_str.encode(), capture_output=True)

errors = [
    line for line in proc.stderr.decode().splitlines()
    if not line.startswith("debugfs 1.")
    and "Allocated inode" not in line
    and "File not found by ext2_lookup while deleting" not in line
    and "File exists while creating directory" not in line
]

if errors:
    print("Encountered debugfs warnings/errors:", file=sys.stderr)
    for err in errors[:20]:
        print(f"  {err}", file=sys.stderr)
    if len(errors) > 20:
        print(f"  ... and {len(errors) - 20} more", file=sys.stderr)
else:
    print("Ext2 filesystem populated successfully without errors.")

PYEOF

echo "Done! disk-ubuntu.img is ready."
