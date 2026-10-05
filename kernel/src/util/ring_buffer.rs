// Copyright (C) 2026 KontsnorOS Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Lock-free ring buffer.
//!
//! A fixed-size circular buffer for single-producer, single-consumer
//! scenarios (e.g., interrupt handler → kernel thread communication).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

/// A lock-free single-producer single-consumer ring buffer.
pub struct RingBuffer<T, const N: usize> {
    buffer: UnsafeCell<[core::mem::MaybeUninit<T>; N]>,
    head: AtomicUsize, // Write position (producer)
    tail: AtomicUsize, // Read position (consumer)
}

// SAFETY: Synchronization of element access is guaranteed by atomic head/tail orderings for SPSC operations.
unsafe impl<T: Send, const N: usize> Sync for RingBuffer<T, N> {}
unsafe impl<T: Send, const N: usize> Send for RingBuffer<T, N> {}

impl<T: Copy, const N: usize> RingBuffer<T, N> {
    /// Create a new, empty ring buffer.
    ///
    /// # Note
    ///
    /// N must be a power of 2 for correct operation.
    pub const fn new() -> Self {
        assert!(N.is_power_of_two(), "Ring buffer size must be a power of 2");
        Self {
            buffer: UnsafeCell::new([const { core::mem::MaybeUninit::uninit() }; N]),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Push an item into the ring buffer.
    ///
    /// Returns `Err(item)` if the buffer is full.
    pub fn push(&self, item: T) -> Result<(), T> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);

        if (head - tail) >= N {
            return Err(item); // Buffer full
        }

        let index = head & (N - 1);
        // SAFETY: The index is bounded in [0..N) by head & (N - 1) where N is power-of-two.
        // Exclusive producer write access is guaranteed by single-producer contract.
        unsafe {
            let buf_ptr = self.buffer.get();
            let elem_ptr = (*buf_ptr).as_mut_ptr().add(index);
            (*elem_ptr).write(item);
        }

        self.head.store(head + 1, Ordering::Release);
        Ok(())
    }

    /// Pop an item from the ring buffer.
    ///
    /// Returns `None` if the buffer is empty.
    pub fn pop(&self) -> Option<T> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);

        if tail >= head {
            return None; // Buffer empty
        }

        let index = tail & (N - 1);
        // SAFETY: The index is bounded in [0..N) by tail & (N - 1) where N is power-of-two.
        // Exclusive consumer read access is guaranteed by single-consumer contract.
        let item = unsafe {
            let buf_ptr = self.buffer.get();
            let elem_ptr = (*buf_ptr).as_ptr().add(index);
            (*elem_ptr).assume_init()
        };

        self.tail.store(tail + 1, Ordering::Release);
        Some(item)
    }

    /// Check if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        tail >= head
    }

    /// Get the number of items in the buffer.
    pub fn len(&self) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        head.wrapping_sub(tail)
    }

    /// Push a slice of elements into the ring buffer using fast bulk memory copies.
    ///
    /// # Performance
    ///
    /// Replaces byte-by-byte or element-by-element push loops with at most two
    /// contiguous bulk memory copies (`copy_nonoverlapping`), compiling down to hardware
    /// vector / `rep movsb` instructions for maximum throughput in SPSC streams.
    ///
    /// Returns the number of elements successfully pushed.
    pub fn push_slice(&self, src: &[T]) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);

        let occupied = head.wrapping_sub(tail);
        let available = N.saturating_sub(occupied);
        if available == 0 || src.is_empty() {
            return 0;
        }

        let to_write = core::cmp::min(src.len(), available);
        let write_index = head & (N - 1);
        let first_chunk = core::cmp::min(to_write, N - write_index);
        let second_chunk = to_write - first_chunk;

        // SAFETY:
        // - `write_index` and `first_chunk`/`second_chunk` are strictly bounded within `[0..N)`.
        // - Single-producer invariant guarantees exclusive producer write access to `buffer[head..head+to_write]`.
        // - `T: Copy` ensures bitwise memory duplication (`copy_nonoverlapping`) is safe.
        unsafe {
            let buf_ptr = (*self.buffer.get()).as_mut_ptr() as *mut T;
            core::ptr::copy_nonoverlapping(src.as_ptr(), buf_ptr.add(write_index), first_chunk);
            if second_chunk > 0 {
                core::ptr::copy_nonoverlapping(
                    src.as_ptr().add(first_chunk),
                    buf_ptr,
                    second_chunk,
                );
            }
        }

        self.head
            .store(head.wrapping_add(to_write), Ordering::Release);
        to_write
    }

    /// Pop elements from the ring buffer into a target slice using fast bulk memory copies.
    ///
    /// # Performance
    ///
    /// Replaces byte-by-byte or element-by-element pop loops with at most two
    /// contiguous bulk memory copies (`copy_nonoverlapping`), maximizing read performance.
    ///
    /// Returns the number of elements successfully popped.
    pub fn pop_slice(&self, dst: &mut [T]) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);

        let available = head.wrapping_sub(tail);
        if available == 0 || dst.is_empty() {
            return 0;
        }

        let to_read = core::cmp::min(dst.len(), available);
        let read_index = tail & (N - 1);
        let first_chunk = core::cmp::min(to_read, N - read_index);
        let second_chunk = to_read - first_chunk;

        // SAFETY:
        // - `read_index` and `first_chunk`/`second_chunk` are strictly bounded within `[0..N)`.
        // - Single-consumer invariant guarantees exclusive consumer read access to `buffer[tail..tail+to_read]`.
        // - `T: Copy` ensures bitwise memory duplication (`copy_nonoverlapping`) is safe.
        unsafe {
            let buf_ptr = (*self.buffer.get()).as_ptr() as *const T;
            core::ptr::copy_nonoverlapping(buf_ptr.add(read_index), dst.as_mut_ptr(), first_chunk);
            if second_chunk > 0 {
                core::ptr::copy_nonoverlapping(
                    buf_ptr,
                    dst.as_mut_ptr().add(first_chunk),
                    second_chunk,
                );
            }
        }

        self.tail
            .store(tail.wrapping_add(to_read), Ordering::Release);
        to_read
    }
}
