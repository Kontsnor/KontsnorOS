//! Sleeping mutex implementation.
//!
//! Unlike a spinlock, a sleeping mutex puts the waiting thread to sleep
//! rather than busy-waiting. This is more efficient for longer critical
//! sections or when the lock is expected to be held for a while.
//!
//! Note: In the current implementation, this falls back to spinning
//! since we don't yet have a full thread scheduler with wait queues.
//! It will be upgraded to a proper sleeping mutex when the scheduler
//! supports blocking.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// A sleeping mutex.
///
/// Currently implemented as a spinning mutex (TODO: convert to sleeping
/// once the scheduler supports wait queues).
pub struct KMutex<T> {
    /// Lock state: true = locked, false = unlocked.
    locked: AtomicBool,
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
            data: UnsafeCell::new(data),
        }
    }

    /// Acquire the mutex.
    ///
    /// Currently spins; will block the thread once the scheduler
    /// supports wait queues.
    pub fn lock(&self) -> KMutexGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // TODO: Add to wait queue and yield to scheduler
            core::hint::spin_loop();
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
        // TODO: Wake up one thread from the wait queue
    }
}
