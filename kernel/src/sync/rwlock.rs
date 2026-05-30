//! Reader-writer lock.
//!
//! Allows multiple concurrent readers OR a single exclusive writer.
//! This is useful for data structures that are read frequently but
//! written infrequently (e.g., the mount table, driver registry).

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicI64, Ordering};

/// A reader-writer lock.
///
/// - Positive count = number of active readers
/// - Zero = unlocked
/// - -1 = locked by a writer
pub struct KRwLock<T> {
    /// Lock state: >0 = readers, 0 = free, -1 = writer.
    state: AtomicI64,
    /// The protected data.
    data: UnsafeCell<T>,
}

// SAFETY: KRwLock provides proper synchronization.
unsafe impl<T: Send> Send for KRwLock<T> {}
unsafe impl<T: Send + Sync> Sync for KRwLock<T> {}

impl<T> KRwLock<T> {
    /// Create a new, unlocked reader-writer lock.
    pub const fn new(data: T) -> Self {
        Self {
            state: AtomicI64::new(0),
            data: UnsafeCell::new(data),
        }
    }

    /// Acquire a read lock.
    pub fn read(&self) -> KRwLockReadGuard<'_, T> {
        loop {
            let state = self.state.load(Ordering::Relaxed);
            if state >= 0 {
                if self
                    .state
                    .compare_exchange_weak(state, state + 1, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    return KRwLockReadGuard { lock: self };
                }
            }
            core::hint::spin_loop();
        }
    }

    /// Acquire a write lock.
    pub fn write(&self) -> KRwLockWriteGuard<'_, T> {
        while self
            .state
            .compare_exchange_weak(0, -1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }

        KRwLockWriteGuard { lock: self }
    }
}

/// RAII guard for a read lock.
pub struct KRwLockReadGuard<'a, T> {
    lock: &'a KRwLock<T>,
}

impl<T> Deref for KRwLockReadGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> Drop for KRwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.state.fetch_sub(1, Ordering::Release);
    }
}

/// RAII guard for a write lock.
pub struct KRwLockWriteGuard<'a, T> {
    lock: &'a KRwLock<T>,
}

impl<T> Deref for KRwLockWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for KRwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for KRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.state.store(0, Ordering::Release);
    }
}
