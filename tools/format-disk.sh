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

echo "Creating 1.5GB blank disk image..."
dd if=/dev/zero of="$DISK_IMG" bs=1M count=1536

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
mkdir /usr/bin
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

echo "Compiling dynamic hello binary..."
musl-gcc -o /tmp/hello_dyn /tmp/hello.c

echo "Compiling stubs library..."
cat << 'EOF' > /tmp/stubs.c
#include <string.h>
#include <stddef.h>
#include <unistd.h>

struct {
    unsigned int __cpu_vendor;
    unsigned int __cpu_type;
    unsigned int __cpu_subtype;
    unsigned int __cpu_features[8];
} __cpu_model = {0};

void __cpu_indicator_init(void) {
    write(2, "STUB: __cpu_indicator_init\n", 27);
}

void *__memset_chk(void *dest, int c, size_t len, size_t destlen) {
    write(2, "STUB: __memset_chk\n", 19);
    return memset(dest, c, len);
}

int _dl_find_object(void *address, void *result) {
    write(2, "STUB: _dl_find_object\n", 22);
    return -1;
}
EOF
musl-gcc -shared -fPIC -o /tmp/libstubs.so /tmp/stubs.c

echo "mkdir /lib" >> "$CMD_FILE"
echo "write /usr/lib/ld-musl-x86_64.so.1 /lib/ld-musl-x86_64.so.1" >> "$CMD_FILE"
echo "write /usr/lib/x86_64-linux-musl/libc.so /lib/libc.so" >> "$CMD_FILE"
echo "write /tmp/hello_dyn /bin/hello_dyn" >> "$CMD_FILE"
echo "write /tmp/libstubs.so /lib/libstubs.so" >> "$CMD_FILE"

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

echo "Writing /compile.sh..."
cat << 'EOF' > /tmp/compile.sh
#!/bin/sh
echo "Compiling hello.rs..."
/usr/bin/rustc -C linker-flavor=ld.lld -C linker=/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld /hello.rs -o /hello_rust
echo "Compilation finished!"
EOF
chmod +x /tmp/compile.sh
echo "write /tmp/compile.sh /compile.sh" >> "$CMD_FILE"

# Copy Rust Toolchain
RUST_TOOLCHAIN_DIR="/home/kontsnor/.rustup/toolchains/nightly-x86_64-unknown-linux-musl"
if [ -d "$RUST_TOOLCHAIN_DIR" ]; then
    echo "Staging Rust toolchain binaries and libraries..."
    echo "mkdir /lib/rustlib" >> "$CMD_FILE"
    echo "mkdir /lib/rustlib/x86_64-unknown-linux-musl" >> "$CMD_FILE"
    echo "mkdir /lib/rustlib/x86_64-unknown-linux-musl/lib" >> "$CMD_FILE"
    echo "mkdir /lib/rustlib/x86_64-unknown-linux-musl/lib/self-contained" >> "$CMD_FILE"
    echo "mkdir /lib/rustlib/x86_64-unknown-linux-musl/bin" >> "$CMD_FILE"
    
    echo "write $RUST_TOOLCHAIN_DIR/bin/rustc /usr/bin/rustc" >> "$CMD_FILE"
    echo "write $RUST_TOOLCHAIN_DIR/bin/cargo /usr/bin/cargo" >> "$CMD_FILE"
    echo "write $RUST_TOOLCHAIN_DIR/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld /lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld" >> "$CMD_FILE"
    
    # Copy librustc_driver shared library
    find "$RUST_TOOLCHAIN_DIR/lib" -maxdepth 1 -name "librustc_driver-*.so" | while read -r filepath; do
        filename=$(basename "$filepath")
        echo "write $filepath /lib/$filename" >> "$CMD_FILE"
    done
    
    # Compile a musl-compatible libgcc_s.so.1 stub library
    cat << 'EOF' > /tmp/libgcc_s.c
#include <unistd.h>
void _Unwind_Backtrace() { write(2, "STUB: _Unwind_Backtrace\n", 24); }
void _Unwind_DeleteException() { write(2, "STUB: _Unwind_DeleteException\n", 30); }
void _Unwind_FindEnclosingFunction() { write(2, "STUB: _Unwind_FindEnclosingFunction\n", 36); }
void _Unwind_GetCFA() { write(2, "STUB: _Unwind_GetCFA\n", 21); }
void _Unwind_GetDataRelBase() { write(2, "STUB: _Unwind_GetDataRelBase\n", 29); }
void _Unwind_GetIP() { write(2, "STUB: _Unwind_GetIP\n", 20); }
void _Unwind_GetIPInfo() { write(2, "STUB: _Unwind_GetIPInfo\n", 24); }
void _Unwind_GetLanguageSpecificData() { write(2, "STUB: _Unwind_GetLanguageSpecificData\n", 38); }
void _Unwind_GetRegionStart() { write(2, "STUB: _Unwind_GetRegionStart\n", 29); }
void _Unwind_GetTextRelBase() { write(2, "STUB: _Unwind_GetTextRelBase\n", 29); }
void _Unwind_RaiseException() { write(2, "STUB: _Unwind_RaiseException\n", 29); }
void _Unwind_Resume() { write(2, "STUB: _Unwind_Resume\n", 21); }
void _Unwind_Resume_or_Rethrow() { write(2, "STUB: _Unwind_Resume_or_Rethrow\n", 32); }
void _Unwind_SetGR() { write(2, "STUB: _Unwind_SetGR\n", 20); }
void _Unwind_SetIP() { write(2, "STUB: _Unwind_SetIP\n", 20); }
void __register_frame_info() { write(2, "STUB: __register_frame_info\n", 28); }
void __deregister_frame_info() { write(2, "STUB: __deregister_frame_info\n", 30); }
int __popcountdi2(unsigned long long a) {
    int count = 0;
    while (a) {
        count += (a & 1);
        a >>= 1;
    }
    return count;
}
EOF

    cat << 'EOF' > /tmp/version.map
GCC_3.0 {
  global:
    _Unwind_DeleteException;
    _Unwind_GetDataRelBase;
    _Unwind_GetLanguageSpecificData;
    _Unwind_GetRegionStart;
    _Unwind_GetTextRelBase;
    _Unwind_RaiseException;
    _Unwind_Resume;
    _Unwind_SetGR;
    _Unwind_SetIP;
    _Unwind_Backtrace;
    _Unwind_FindEnclosingFunction;
    _Unwind_GetCFA;
    _Unwind_GetIP;
    __register_frame_info;
    __deregister_frame_info;
  local:
    *;
};

GCC_3.3 {
  global:
    _Unwind_Resume_or_Rethrow;
} GCC_3.0;

GCC_3.4 {
  global:
    __popcountdi2;
} GCC_3.3;

GCC_4.2.0 {
  global:
    _Unwind_GetIPInfo;
} GCC_3.4;
EOF

    musl-gcc -shared -fPIC -o /tmp/libgcc_s.so.1 /tmp/libgcc_s.c -Wl,--version-script=/tmp/version.map
    echo "write /tmp/libgcc_s.so.1 /lib/libgcc_s.so.1" >> "$CMD_FILE"
    
    # Copy target stdlib files
    STDLIB_SRC_DIR="$RUST_TOOLCHAIN_DIR/lib/rustlib/x86_64-unknown-linux-musl/lib"
    find "$STDLIB_SRC_DIR" -type f | while read -r filepath; do
        relpath="${filepath#$STDLIB_SRC_DIR/}"
        echo "write $filepath /lib/rustlib/x86_64-unknown-linux-musl/lib/$relpath" >> "$CMD_FILE"
    done
else
    echo "WARNING: Rust toolchain dir $RUST_TOOLCHAIN_DIR not found!"
fi

# Stage KontsnorOS kernel source code
echo "Staging KontsnorOS kernel source code..."
echo "mkdir /src" >> "$CMD_FILE"
echo "mkdir /src/KontsnorOS" >> "$CMD_FILE"

# Pre-create all subdirectories under kernel, driver-sdk, and tools
find "$PROJECT_DIR/kernel" "$PROJECT_DIR/driver-sdk" "$PROJECT_DIR/tools" -type d | while read -r dirpath; do
    relpath="${dirpath#$PROJECT_DIR/}"
    echo "mkdir /src/KontsnorOS/$relpath" >> "$CMD_FILE"
done

# Copy Cargo.toml, Cargo.lock, rust-toolchain.toml
echo "write $PROJECT_DIR/Cargo.toml /src/KontsnorOS/Cargo.toml" >> "$CMD_FILE"
if [ -f "$PROJECT_DIR/Cargo.lock" ]; then
    echo "write $PROJECT_DIR/Cargo.lock /src/KontsnorOS/Cargo.lock" >> "$CMD_FILE"
fi
if [ -f "$PROJECT_DIR/rust-toolchain.toml" ]; then
    echo "write $PROJECT_DIR/rust-toolchain.toml /src/KontsnorOS/rust-toolchain.toml" >> "$CMD_FILE"
fi

# Copy all source files under kernel, driver-sdk, and tools
find "$PROJECT_DIR/kernel" "$PROJECT_DIR/driver-sdk" "$PROJECT_DIR/tools" -type f | while read -r filepath; do
    relpath="${filepath#$PROJECT_DIR/}"
    echo "write $filepath /src/KontsnorOS/$relpath" >> "$CMD_FILE"
done

# Write hello.rs
echo 'fn main() { println!("Hello World from native Rust compiled binary on KontsnorOS!"); }' > /tmp/hello.rs
echo "write /tmp/hello.rs /hello.rs" >> "$CMD_FILE"

# Execute debugfs
debugfs -w "$DISK_IMG" -f "$CMD_FILE" >/dev/null

rm -f "$CMD_FILE" /tmp/hello.c /tmp/install-busybox.sh /tmp/hello.txt /tmp/hello_dyn /tmp/hello.rs /tmp/stubs.c /tmp/libstubs.so /tmp/libgcc_s.c /tmp/libgcc_s.so.1

echo "Done! disk.img is ready."

