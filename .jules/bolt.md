# Bolt's Journal

## 2026-03-30 - Physical Frame Allocator Bitmap Bitwise Search Optimization
**Learning:** In `kernel/src/memory/physical.rs`, scanning 64-frame bitmap blocks byte-by-byte or bit-by-bit when `val != u64::MAX` introduces up to 63 loop iterations per allocation. Utilizing `(!val).trailing_zeros()` resolves the first available free frame in O(1) cycle time via x86 `TZCNT`/`BSF` hardware instructions.
**Action:** Always check bitwise operations like `trailing_zeros()` or `trailing_ones()` when working with bitmap allocators or bitmasks to avoid redundant loop iterations on non-full words.
