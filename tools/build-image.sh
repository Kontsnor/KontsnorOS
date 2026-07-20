#!/bin/bash
# KontsnorOS — Build bootable disk image
#
# Usage: ./tools/build-image.sh [--release]
#

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

BUILD_TYPE="debug"
CARGO_ARGS=()

for arg in "$@"; do
    case "$arg" in
        --release)
            BUILD_TYPE="release"
            CARGO_ARGS+=("--release")
            ;;
        *)
            CARGO_ARGS+=("$arg")
            ;;
    esac
done

# Compile the kernel first
echo "Compiling kernel ($BUILD_TYPE)..."
cargo build "${CARGO_ARGS[@]}"

KERNEL_BIN="$PROJECT_DIR/target/x86_64-unknown-none/$BUILD_TYPE/kontsnor-kernel"

if [ ! -f "$KERNEL_BIN" ]; then
    echo "Kernel binary not found at: $KERNEL_BIN"
    exit 1
fi

echo "Building bootable disk image..."
STRIPPED_DIR="$PROJECT_DIR/target/stripped"
mkdir -p "$STRIPPED_DIR"
cp "$KERNEL_BIN" "$STRIPPED_DIR/kontsnor-kernel"
strip "$STRIPPED_DIR/kontsnor-kernel"
bootloader_linker build "$STRIPPED_DIR/kontsnor-kernel" -o "$PROJECT_DIR" -s

echo "Bootable disk image built successfully at $PROJECT_DIR/bios.img."
