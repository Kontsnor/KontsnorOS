#!/bin/bash
# Formats disk.img with ext2 and injects bash, sh, busybox, and hello.txt
set -e

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DISK_IMG="$PROJECT_DIR/disk.img"
BASH_BIN="/tmp/bash-build/bash-5.2.21/bash"
SH_BIN="$PROJECT_DIR/tools/sh"
BUSYBOX_BIN="$PROJECT_DIR/busybox-build/busybox-1.36.1/busybox"

if [ ! -f "$BASH_BIN" ]; then
    echo "Compiled bash binary not found at $BASH_BIN"
    exit 1
fi

if [ ! -f "$SH_BIN" ]; then
    echo "sh binary not found at $SH_BIN"
    exit 1
fi

if [ ! -f "$BUSYBOX_BIN" ]; then
    echo "BusyBox binary not found at $BUSYBOX_BIN"
    exit 1
fi

echo "Creating 64MB blank disk image..."
dd if=/dev/zero of="$DISK_IMG" bs=1M count=64

echo "Formatting disk.img with ext2 (4096 byte blocks)..."
mkfs.ext2 -b 4096 -F "$DISK_IMG"

echo "Making binaries executable on host..."
chmod +x "$BASH_BIN"
chmod +x "$SH_BIN"
chmod +x "$BUSYBOX_BIN"

echo "Writing directories and files to disk.img via debugfs..."
debugfs -w "$DISK_IMG" -R "mkdir /bin"
debugfs -w "$DISK_IMG" -R "mkdir /etc"
debugfs -w "$DISK_IMG" -R "mkdir /tmp"
debugfs -w "$DISK_IMG" -R "mkdir /usr"

debugfs -w "$DISK_IMG" -R "write $BASH_BIN /bin/bash"
debugfs -w "$DISK_IMG" -R "write $SH_BIN /bin/sh"
debugfs -w "$DISK_IMG" -R "write $BUSYBOX_BIN /bin/busybox"

echo "Writing /bin/install-busybox.sh..."
echo "#!/bin/bash
/bin/busybox --install -s /bin
echo \"BusyBox installed!\"" > /tmp/install-busybox.sh
debugfs -w "$DISK_IMG" -R "write /tmp/install-busybox.sh /bin/install-busybox.sh"
rm /tmp/install-busybox.sh

echo "Writing /hello.txt..."
echo "Hello from the ext2 disk on KontsnorOS!" > /tmp/hello.txt
debugfs -w "$DISK_IMG" -R "write /tmp/hello.txt /hello.txt"
rm /tmp/hello.txt

echo "Done! disk.img is ready."
