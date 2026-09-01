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

echo "Creating 6GB blank disk image..."
dd if=/dev/zero of="$DISK_IMG" bs=1M count=6144

echo "Formatting disk.img with ext2 (4096 byte blocks)..."
mkfs.ext2 -b 4096 -F "$DISK_IMG"

echo "Making binaries executable on host..."
chmod +x "$SH_BIN"
if [ -f "$BUSYBOX_BIN" ]; then
    chmod +x "$BUSYBOX_BIN"
fi
chmod +x "$INIT_BIN"
if [ -f "$BASH_BIN" ]; then
    chmod +x "$BASH_BIN"
fi

echo "Writing directories, headers, libraries and files to disk.img via debugfs..."
CMD_FILE=$(mktemp)

ALPINE_TAR="/tmp/alpine-minirootfs-3.20.0-x86_64.tar.gz"
ALPINE_STAGE="/tmp/alpine-stage"

if [ ! -f "$ALPINE_TAR" ]; then
    echo "Downloading Alpine Linux minirootfs..."
    wget -O "$ALPINE_TAR" https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/x86_64/alpine-minirootfs-3.20.0-x86_64.tar.gz
fi

if [ ! -d "$ALPINE_STAGE" ]; then
    echo "Extracting Alpine Linux minirootfs..."
    rm -rf "$ALPINE_STAGE"
    mkdir -p "$ALPINE_STAGE"
    tar -xzf "$ALPINE_TAR" -C "$ALPINE_STAGE"
fi

# 1. Directories from Alpine rootfs
find "$ALPINE_STAGE" -type d | sort | while read -r dirpath; do
    relpath="${dirpath#$ALPINE_STAGE}"
    if [ -n "$relpath" ]; then
        echo "mkdir $relpath" >> "$CMD_FILE"
    fi
done

# Ensure extra non-Alpine directories required by compiler/TCC/toolchain are present
for dir in /usr/lib/tcc /usr/lib/tcc/include /usr/include /usr/lib/x86_64-linux-gnu /usr/include/x86_64-linux-gnu; do
    echo "mkdir $dir" >> "$CMD_FILE"
done

# Create nested directories for musl headers
for dir in net bits scsi netinet sys netpacket arpa; do
    echo "mkdir /usr/include/$dir" >> "$CMD_FILE"
done

# 2. Files from Alpine rootfs
find "$ALPINE_STAGE" -type f | while read -r filepath; do
    relpath="${filepath#$ALPINE_STAGE}"
    echo "write $filepath $relpath" >> "$CMD_FILE"
done

# 3. Symbolic links from Alpine rootfs
find "$ALPINE_STAGE" -type l | while read -r linkpath; do
    relpath="${linkpath#$ALPINE_STAGE}"
    target=$(readlink "$linkpath")
    echo "symlink $relpath $target" >> "$CMD_FILE"
done

# Delete existing init and library symlinks in rootfs so we can write our custom versions
echo "rm /sbin/init" >> "$CMD_FILE"
echo "rm /lib/ld-musl-x86_64.so.1" >> "$CMD_FILE"
echo "rm /lib/libc.so" >> "$CMD_FILE"

# Copy bash, sh, custom init
if [ -f "$BASH_BIN" ]; then
    echo "write $BASH_BIN /bin/bash" >> "$CMD_FILE"
fi
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

echo "Writing /build_cargo.sh..."
cat << 'EOF' > /tmp/build_cargo.sh
#!/bin/sh
echo "Starting Native Cargo Build inside KontsnorOS..."
cd /src/KontsnorOS || cd /disk/src/KontsnorOS || exit 1
export CARGO_TARGET_DIR=/tmp/target
mkdir -p /tmp/target
export CARGO_BUILD_JOBS=8
export RUSTC=/usr/bin/rustc
export RUSTFLAGS="-C linker-flavor=ld.lld -C linker=/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld -C codegen-units=8 -C lto=off -C debuginfo=0 -C opt-level=0"
export RUSTFLAGS_BOOTSTRAP="-C linker-flavor=ld.lld -C linker=/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld -C codegen-units=8"
export CARGO_HOST_RUSTFLAGS="-C linker-flavor=ld.lld -C linker=/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld -C codegen-units=8 -C lto=off -C debuginfo=0 -C opt-level=0"
export HOST_RUSTFLAGS="-C linker-flavor=ld.lld -C linker=/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld -C codegen-units=8 -C lto=off -C debuginfo=0 -C opt-level=0"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld"
export PATH="/usr/bin:/bin:$PATH"
/usr/bin/cargo build --package kontsnor-kernel --profile fast-build --target x86_64-unknown-linux-musl --offline -j 8 --verbose
STATUS=$?
echo "Cargo build finished with exit status $STATUS"
if [ $STATUS -eq 0 ]; then
    echo "Copying compiled kernel binary to persistent disk..."
    mkdir -p /disk/src/KontsnorOS/target/x86_64-unknown-linux-musl/fast-build/
    cp /tmp/target/x86_64-unknown-linux-musl/fast-build/kontsnor-kernel /disk/src/KontsnorOS/target/x86_64-unknown-linux-musl/fast-build/ 2>/dev/null
fi
sync
ls -lh /tmp/target/x86_64-unknown-linux-musl/fast-build/kontsnor-kernel /disk/src/KontsnorOS/target/x86_64-unknown-linux-musl/fast-build/kontsnor-kernel 2>/dev/null
EOF
chmod +x /tmp/build_cargo.sh
echo "write /tmp/build_cargo.sh /build_cargo.sh" >> "$CMD_FILE"

# Copy Rust Toolchain
RUST_TOOLCHAIN_DIR="/home/kontsnor/.rustup/toolchains/nightly-x86_64-unknown-linux-musl"
if [ -d "$RUST_TOOLCHAIN_DIR" ]; then
    echo "Staging Rust toolchain binaries and libraries..."
    echo "mkdir /lib/rustlib" >> "$CMD_FILE"
    echo "mkdir /lib/rustlib/x86_64-unknown-linux-musl" >> "$CMD_FILE"
    echo "mkdir /lib/rustlib/x86_64-unknown-linux-musl/lib" >> "$CMD_FILE"
    echo "mkdir /lib/rustlib/x86_64-unknown-linux-musl/lib/self-contained" >> "$CMD_FILE"
    echo "mkdir /lib/rustlib/x86_64-unknown-linux-musl/bin" >> "$CMD_FILE"
    echo "mkdir /lib/rustlib/x86_64-unknown-linux-musl/bin/gcc-ld" >> "$CMD_FILE"
    echo "mkdir /lib/rustlib/x86_64-unknown-linux-musl/bin/self-contained" >> "$CMD_FILE"
    
    echo "write $RUST_TOOLCHAIN_DIR/bin/rustc /usr/bin/rustc_real" >> "$CMD_FILE"
    echo "write $RUST_TOOLCHAIN_DIR/bin/cargo /usr/bin/cargo" >> "$CMD_FILE"
    echo "write $RUST_TOOLCHAIN_DIR/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld /lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld" >> "$CMD_FILE"
    echo "write $RUST_TOOLCHAIN_DIR/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld /usr/bin/rust-lld" >> "$CMD_FILE"

    # Create rustc wrapper that guarantees default linker-flavor=ld.lld and codegen-units=8 for ALL compilations (including build.rs)
    cat << 'EOF' > /tmp/rustc_wrapper.sh
#!/bin/sh
exec /usr/bin/rustc_real -C linker-flavor=ld.lld -C linker=/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld -C codegen-units=8 -C lto=off -C debuginfo=0 -C opt-level=0 "$@"
EOF
    chmod +x /tmp/rustc_wrapper.sh
    echo "write /tmp/rustc_wrapper.sh /usr/bin/rustc" >> "$CMD_FILE"
    echo "write /tmp/rustc_wrapper.sh /bin/rustc" >> "$CMD_FILE"

    # Create ld wrapper that ensures any direct GNU ld invocation redirects to rust-lld -flavor gnu
    cat << 'EOF' > /tmp/ld_wrapper.sh
#!/bin/sh
exec /lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld -flavor gnu "$@"
EOF
    chmod +x /tmp/ld_wrapper.sh
    echo "write /tmp/ld_wrapper.sh /usr/bin/ld" >> "$CMD_FILE"
    echo "write /tmp/ld_wrapper.sh /bin/ld" >> "$CMD_FILE"
    echo "write /tmp/ld_wrapper.sh /lib/rustlib/x86_64-unknown-linux-musl/bin/ld" >> "$CMD_FILE"
    echo "write /tmp/ld_wrapper.sh /lib/rustlib/x86_64-unknown-linux-musl/bin/gcc-ld/ld" >> "$CMD_FILE"

    # Copy gcc-ld binaries if present
    if [ -d "$RUST_TOOLCHAIN_DIR/lib/rustlib/x86_64-unknown-linux-musl/bin/gcc-ld" ]; then
        find "$RUST_TOOLCHAIN_DIR/lib/rustlib/x86_64-unknown-linux-musl/bin/gcc-ld" -type f | while read -r filepath; do
            filename=$(basename "$filepath")
            echo "write $filepath /lib/rustlib/x86_64-unknown-linux-musl/bin/gcc-ld/$filename" >> "$CMD_FILE"
        done
    fi

    # Create cc and gcc symlinks pointing to tcc for C compiler invocations
    echo "symlink /usr/bin/cc /bin/tcc" >> "$CMD_FILE"
    echo "symlink /bin/cc /bin/tcc" >> "$CMD_FILE"
    echo "symlink /usr/bin/gcc /bin/tcc" >> "$CMD_FILE"
    echo "symlink /bin/gcc /bin/tcc" >> "$CMD_FILE"
    echo "symlink /lib/rustlib/x86_64-unknown-linux-musl/bin/cc /bin/tcc" >> "$CMD_FILE"
    echo "symlink /lib/rustlib/x86_64-unknown-linux-musl/bin/gcc /bin/tcc" >> "$CMD_FILE"
    echo "symlink /lib/rustlib/x86_64-unknown-linux-musl/bin/self-contained/cc /bin/tcc" >> "$CMD_FILE"
    
    # Copy librustc_driver shared library
    find "$RUST_TOOLCHAIN_DIR/lib" -maxdepth 1 -name "librustc_driver-*.so" | while read -r filepath; do
        filename=$(basename "$filepath")
        echo "write $filepath /lib/$filename" >> "$CMD_FILE"
    done

    # Copy Cargo registry cache
    CARGO_REGISTRY_DIR="/home/kontsnor/.cargo/registry"
    if [ -d "$CARGO_REGISTRY_DIR" ]; then
        echo "Staging Cargo registry cache..."
        echo "mkdir /root" >> "$CMD_FILE"
        echo "mkdir /root/.cargo" >> "$CMD_FILE"
        find "$CARGO_REGISTRY_DIR" -type d | while read -r dirpath; do
            relpath="${dirpath#$CARGO_REGISTRY_DIR/}"
            if [ "$dirpath" != "$CARGO_REGISTRY_DIR" ]; then
                echo "mkdir /root/.cargo/registry/$relpath" >> "$CMD_FILE"
            else
                echo "mkdir /root/.cargo/registry" >> "$CMD_FILE"
            fi
        done
        find "$CARGO_REGISTRY_DIR" -type f | while read -r filepath; do
            relpath="${filepath#$CARGO_REGISTRY_DIR/}"
            echo "write $filepath /root/.cargo/registry/$relpath" >> "$CMD_FILE"
        done
    fi
    
    # Ensure real Alpine musl libgcc_s.so.1 library is present
    if [ ! -f "/tmp/alpine_libgcc/usr/lib/libgcc_s.so.1" ]; then
        mkdir -p /tmp/alpine_libgcc
        curl -sL https://dl-cdn.alpinelinux.org/alpine/v3.20/main/x86_64/libgcc-13.2.1_git20240309-r1.apk | tar -xz -C /tmp/alpine_libgcc 2>/dev/null || true
    fi
    if [ -f "/tmp/alpine_libgcc/usr/lib/libgcc_s.so.1" ]; then
        echo "write /tmp/alpine_libgcc/usr/lib/libgcc_s.so.1 /lib/libgcc_s.so.1" >> "$CMD_FILE"
        echo "write /tmp/alpine_libgcc/usr/lib/libgcc_s.so.1 /usr/lib/libgcc_s.so.1" >> "$CMD_FILE"
    fi
    echo "symlink /lib/libc.musl-x86_64.so.1 /lib/ld-musl-x86_64.so.1" >> "$CMD_FILE"
    
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

# Copy Cargo.toml, Cargo.lock, rust-toolchain.toml, .cargo/config.toml
echo "write $PROJECT_DIR/Cargo.toml /src/KontsnorOS/Cargo.toml" >> "$CMD_FILE"
if [ -f "$PROJECT_DIR/Cargo.lock" ]; then
    echo "write $PROJECT_DIR/Cargo.lock /src/KontsnorOS/Cargo.lock" >> "$CMD_FILE"
fi
if [ -f "$PROJECT_DIR/rust-toolchain.toml" ]; then
    echo "write $PROJECT_DIR/rust-toolchain.toml /src/KontsnorOS/rust-toolchain.toml" >> "$CMD_FILE"
fi

cat << 'EOF' > /tmp/guest_cargo_config.toml
[build]
rustflags = [
    "-C", "linker-flavor=ld.lld",
    "-C", "linker=/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld",
    "-C", "codegen-units=8",
    "-C", "lto=off",
    "-C", "debuginfo=0",
    "-C", "opt-level=0",
]

[host]
rustflags = [
    "-C", "linker-flavor=ld.lld",
    "-C", "linker=/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld",
    "-C", "codegen-units=8",
    "-C", "lto=off",
    "-C", "debuginfo=0",
    "-C", "opt-level=0",
]

[target.'cfg(all())']
rustflags = [
    "-C", "linker-flavor=ld.lld",
    "-C", "linker=/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld",
    "-C", "codegen-units=8",
    "-C", "lto=off",
    "-C", "debuginfo=0",
    "-C", "opt-level=0",
]

[target.x86_64-unknown-linux-musl]
linker = "/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld"
rustflags = [
    "-C", "linker-flavor=ld.lld",
    "-C", "linker=/lib/rustlib/x86_64-unknown-linux-musl/bin/rust-lld",
    "-C", "codegen-units=8",
    "-C", "lto=off",
    "-C", "debuginfo=0",
    "-C", "opt-level=0",
]
EOF
echo "mkdir /src/KontsnorOS/.cargo" >> "$CMD_FILE"
echo "write /tmp/guest_cargo_config.toml /src/KontsnorOS/.cargo/config.toml" >> "$CMD_FILE"

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

rm -f "$CMD_FILE" /tmp/hello.c /tmp/install-busybox.sh /tmp/hello.txt /tmp/hello_dyn /tmp/hello.rs /tmp/stubs.c /tmp/libstubs.so /tmp/libgcc_s.c /tmp/libgcc_s.so.1 /tmp/build_cargo.sh

echo "Done! disk.img is ready."

