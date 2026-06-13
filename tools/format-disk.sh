#!/bin/bash
# Formats disk.img with ext2 and injects bash, sh, busybox, hello.txt, and init
set -e

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DISK_IMG="$PROJECT_DIR/disk.img"
BASH_BIN="/tmp/bash-build/bash-5.2.21/bash"
SH_BIN="$PROJECT_DIR/tools/sh"
BUSYBOX_BIN="$PROJECT_DIR/busybox-build/busybox-1.36.1/busybox"
INIT_BIN="$PROJECT_DIR/tools/init"

echo "Compiling init binary..."
musl-gcc -static -nostdlib -o "$INIT_BIN" "$PROJECT_DIR/tools/init.c"

echo "Compiling sh binary..."
musl-gcc -static -nostdlib -o "$SH_BIN" "$PROJECT_DIR/tools/sh.c"

TCC_DIR="$PROJECT_DIR/tcc-build"
TCC_BIN="$TCC_DIR/tcc"
if [ ! -f "$TCC_BIN" ]; then
    echo "tcc binary not found. Cloning and compiling statically..."
    git clone --depth 1 https://github.com/TinyCC/tinycc.git "$TCC_DIR"
    (
        cd "$TCC_DIR"
        ./configure --prefix=/usr --cc=musl-gcc --extra-cflags="-static" --extra-ldflags="-static" --enable-static
        make -j$(nproc)
    )
fi

echo "Creating 64MB blank disk image..."
dd if=/dev/zero of="$DISK_IMG" bs=1M count=64

echo "Formatting disk.img with ext2 (4096 byte blocks)..."
mkfs.ext2 -b 4096 -F "$DISK_IMG"

echo "Making binaries executable on host..."
chmod +x "$SH_BIN"
chmod +x "$BUSYBOX_BIN"
chmod +x "$INIT_BIN"
if [ -f "$BASH_BIN" ]; then
    chmod +x "$BASH_BIN"
fi

echo "Writing directories, headers, libraries and files to disk.img via debugfs..."
CMD_FILE=$(mktemp)

cat <<EOF > "$CMD_FILE"
mkdir /bin
mkdir /sbin
mkdir /etc
mkdir /tmp
mkdir /var
mkdir /usr
mkdir /usr/include
mkdir /usr/lib
mkdir /usr/lib/tcc
mkdir /usr/lib/tcc/include
mkdir /usr/lib/x86_64-linux-gnu
mkdir /usr/include/x86_64-linux-gnu
EOF

# Create nested directories for musl headers
for dir in net bits scsi netinet sys netpacket arpa; do
    echo "mkdir /usr/include/$dir" >> "$CMD_FILE"
done

# Copy bash, busybox, sh, init
if [ -f "$BASH_BIN" ]; then
    echo "write $BASH_BIN /bin/bash" >> "$CMD_FILE"
fi
echo "write $BUSYBOX_BIN /bin/sh" >> "$CMD_FILE"
echo "write $BUSYBOX_BIN /bin/busybox" >> "$CMD_FILE"
echo "write $SH_BIN /bin/sh_c" >> "$CMD_FILE"
echo "write $INIT_BIN /sbin/init" >> "$CMD_FILE"

# Write tcc binary and runtime libraries/headers
echo "write $TCC_BIN /bin/tcc" >> "$CMD_FILE"
echo "write $TCC_DIR/libtcc1.a /usr/lib/tcc/libtcc1.a" >> "$CMD_FILE"

# Copy TCC internal headers
find "$TCC_DIR/include" -type f | while read -r filepath; do
    relpath="${filepath#$TCC_DIR/include/}"
    echo "write $filepath /usr/lib/tcc/include/$relpath" >> "$CMD_FILE"
done

# Copy musl standard headers
find /usr/include/x86_64-linux-musl -type f | while read -r filepath; do
    relpath="${filepath#/usr/include/x86_64-linux-musl/}"
    echo "write $filepath /usr/include/$relpath" >> "$CMD_FILE"
done

# Copy musl libraries/startup objects
for libfile in libc.a crt1.o crti.o crtn.o; do
    echo "write /usr/lib/x86_64-linux-musl/$libfile /usr/lib/$libfile" >> "$CMD_FILE"
    echo "write /usr/lib/x86_64-linux-musl/$libfile /usr/lib/x86_64-linux-gnu/$libfile" >> "$CMD_FILE"
done

# Create a sample hello.c
cat <<EOF > /tmp/hello.c
#include <stdio.h>
#include <stdlib.h>

int main() {
    printf("Hello World from compiled binary on KontsnorOS!\\n");
    return 0;
}
EOF

echo "write /tmp/hello.c /hello.c" >> "$CMD_FILE"
echo "write $PROJECT_DIR/tools/sh.c /sh.c" >> "$CMD_FILE"

echo "Writing /bin/install-busybox.sh..."
cat <<EOF > /tmp/install-busybox.sh
#!/bin/sh
/bin/busybox --install -s /bin
echo "BusyBox installed!"
EOF
chmod +x /tmp/install-busybox.sh
echo "write /tmp/install-busybox.sh /bin/install-busybox.sh" >> "$CMD_FILE"

echo "Writing /hello.txt..."
echo "Hello from the ext2 disk on KontsnorOS!" > /tmp/hello.txt
echo "write /tmp/hello.txt /hello.txt" >> "$CMD_FILE"

# Execute debugfs
debugfs -w "$DISK_IMG" -f "$CMD_FILE" >/dev/null

rm -f "$CMD_FILE" /tmp/hello.c /tmp/install-busybox.sh /tmp/hello.txt

echo "Done! disk.img is ready."

