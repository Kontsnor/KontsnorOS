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

//! Sleeping mutex implementation.
//!
//! Unlike a spinlock, a sleeping mutex puts the waiting thread to sleep
//! rather than busy-waiting. This is more efficient for longer critical
//! sections or when the lock is expected to be held for a while.
//!
use crate::sync::wait_queue::WaitQueue;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// A sleeping mutex.
///
/// Uses an atomic flag for fast-path uncontended access, a short bounded spin
/// loop for brief lock hold times, and sleeps on a `WaitQueue` when contended.
pub struct KMutex<T> {
    /// Lock state: true = locked, false = unlocked.
    locked: AtomicBool,
    /// Wait queue for tasks waiting to acquire the mutex.
    wait_queue: WaitQueue,
    /// The protected data.
    data: UnsafeCell<T>,
}

// SAFETY: KMutex provides mutual exclusion.
unsafe impl<T: Send> Send for KMutex<T> {}
unsafe impl<T: Send> Sync for KMutex<T> {}

impl<T> KMutex<T> {
    /// Create a new, unlocked mutex.
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            wait_queue: WaitQueue::new(),
            data: UnsafeCell::new(data),
        }
    }

    /// Acquire the mutex.
    ///
    /// Fast path: Attempts an immediate atomic acquire.
    /// Spin path: Spins briefly (16 iterations) using `spin_loop` in case the holder
    /// releases quickly.
    /// Slow path: Enqueues on `wait_queue` and blocks until woken up by `KMutexGuard::drop`.
    pub fn lock(&self) -> KMutexGuard<'_, T> {
        // Fast path
        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return KMutexGuard { mutex: self };
        }

        // Bounded short spin loop
        for _ in 0..16 {
            if self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return KMutexGuard { mutex: self };
            }
            core::hint::spin_loop();
        }

        // Slow path: sleep on wait queue until acquired
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.wait_queue.wait();
        }

        KMutexGuard { mutex: self }
    }

    /// Try to acquire the mutex without blocking.
    pub fn try_lock(&self) -> Option<KMutexGuard<'_, T>> {
        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(KMutexGuard { mutex: self })
        } else {
            None
        }
    }
}

/// RAII guard for a KMutex.
pub struct KMutexGuard<'a, T> {
    mutex: &'a KMutex<T>,
}

impl<T> Deref for KMutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> DerefMut for KMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for KMutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.locked.store(false, Ordering::Release);
        self.mutex.wait_queue.wake_one();
    }
}
