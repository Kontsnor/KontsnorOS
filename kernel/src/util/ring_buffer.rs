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

use core::sync::atomic::{AtomicUsize, Ordering};

/// A lock-free single-producer single-consumer ring buffer.
pub struct RingBuffer<T, const N: usize> {
    buffer: [core::mem::MaybeUninit<T>; N],
    head: AtomicUsize, // Write position (producer)
    tail: AtomicUsize, // Read position (consumer)
}

impl<T: Copy, const N: usize> RingBuffer<T, N> {
    /// Create a new, empty ring buffer.
    ///
    /// # Note
    ///
    /// N must be a power of 2 for correct operation.
    pub const fn new() -> Self {
        assert!(N.is_power_of_two(), "Ring buffer size must be a power of 2");
        Self {
            // SAFETY: MaybeUninit doesn't require initialization
            buffer: unsafe { core::mem::MaybeUninit::uninit().assume_init() },
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
        // SAFETY: We have exclusive producer access and the index is in bounds.
        unsafe {
            let ptr = self.buffer.as_ptr().add(index) as *mut T;
            ptr.write(item);
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
        // SAFETY: We have exclusive consumer access and the index is in bounds.
        let item = unsafe {
            let ptr = self.buffer.as_ptr().add(index) as *const T;
            ptr.read()
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
        head - tail
    }
}
