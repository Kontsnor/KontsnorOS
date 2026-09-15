# Bolt's Journal

## 2026-03-30 - Physical Frame Allocator Bitmap Bitwise Search Optimization
**Learning:** In `kernel/src/memory/physical.rs`, scanning 64-frame bitmap blocks byte-by-byte or bit-by-bit when `val != u64::MAX` introduces up to 63 loop iterations per allocation. Utilizing `(!val).trailing_zeros()` resolves the first available free frame in O(1) cycle time via x86 `TZCNT`/`BSF` hardware instructions.
**Action:** Always check bitwise operations like `trailing_zeros()` or `trailing_ones()` when working with bitmap allocators or bitmasks to avoid redundant loop iterations on non-full words.

## 2026-03-30 - Pipe Buffer Ring-Buffer Bulk Slice Copy Optimization
**Learning:** Performing byte-by-byte loops and modulo operations in `PipeBuffer` (`push`/`pop`) incurs severe CPU overhead and branch mispredictions on large pipe read/write operations (e.g. 64 KiB buffers). Implementing `push_slice` and `pop_slice` with `copy_from_slice` reduces transfer overheads from O(N) loop iterations to at most two O(1) bulk memory copies (`rep movsb`).
**Action:** When working with ring buffers or IPC stream channels, prefer slice-based contiguous chunk copies over element-by-element push/pop loops.

## 2026-03-30 - 32-Byte Unrolled Internet Checksum Optimization
**Learning:** Computing Internet checksums (RFC 1071) byte-by-byte or 16 bits at a time in `internet_checksum` and `compute_transport_checksum` causes 16x excessive loop iterations and branch overhead during network packet processing. Loop unrolling over 32-byte (16 x 16-bit word) blocks dramatically reduces loop branch checks and instruction pipeline stalls without risk of `u32` accumulator overflow or endianness conversion bugs.
**Action:** Use multi-word unrolled loops when calculating 16-bit Internet checksums over packet buffers to minimize loop branch overhead while keeping arithmetic safely within `u32`.
