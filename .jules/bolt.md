# Bolt's Journal

## 2026-03-30 - Physical Frame Allocator Bitmap Bitwise Search Optimization
**Learning:** In `kernel/src/memory/physical.rs`, scanning 64-frame bitmap blocks byte-by-byte or bit-by-bit when `val != u64::MAX` introduces up to 63 loop iterations per allocation. Utilizing `(!val).trailing_zeros()` resolves the first available free frame in O(1) cycle time via x86 `TZCNT`/`BSF` hardware instructions.
**Action:** Always check bitwise operations like `trailing_zeros()` or `trailing_ones()` when working with bitmap allocators or bitmasks to avoid redundant loop iterations on non-full words.

## 2026-03-30 - E1000 Interrupt Mask & Ring Buffer / Socket FIFO Optimizations
**Learning:**
1. In `kernel/src/drivers/net/e1000.rs`, omitting `RXT0` (`0x1000`) in `REG_IMS` prevents hardware interrupts on packet arrival, forcing packet processing to stall until timer interrupts poll the network stack.
2. Updating `REG_RDT` on every single received packet introduces high MMIO PCI write overhead. Batching `REG_RDT` writes once per receive loop (`flush_rx_tail`) drastically improves packet throughput.
3. Using `Vec<u8>` for `tcp_recv_buf` causes $O(N)$ `drain(..n)` memory shifts on every syscall read (moving hundreds of megabytes during large file transfers). Switching to `VecDeque<u8>` converts buffer draining to $O(1)$ pointer arithmetic.
4. Using `first_key_value()` on `BTreeMap` out-of-order TCP queues eliminates $O(N^2)$ key-cloning linear scans during in-order packet processing.
**Action:** Always check interrupt masks, avoid per-packet MMIO register writes / heap allocations in network drivers, use `VecDeque` for stream FIFO buffers, and use direct `first_key_value()` lookups on sorted maps.
