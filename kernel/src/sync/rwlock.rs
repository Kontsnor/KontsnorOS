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

//! Reader-writer lock.
//!
//! Allows multiple concurrent readers OR a single exclusive writer.
//! Disables interrupts while held and cooperatively services TLB shootdowns
//! during spin waits to prevent SMP deadlocks.

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
        let interrupts_enabled = x86_64::instructions::interrupts::are_enabled();
        if interrupts_enabled {
            x86_64::instructions::interrupts::disable();
        }

        loop {
            let state = self.state.load(Ordering::Relaxed);
            if state >= 0 {
                if self
                    .state
                    .compare_exchange_weak(state, state + 1, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    return KRwLockReadGuard {
                        lock: self,
                        interrupts_enabled,
                    };
                }
            }
            if crate::arch::x86_64::smp::has_pending_tlb_shootdown() {
                x86_64::instructions::tlb::flush_all();
                crate::arch::x86_64::smp::tlb_shootdown_ack();
            }
            core::hint::spin_loop();
        }
    }

    /// Acquire a write lock.
    pub fn write(&self) -> KRwLockWriteGuard<'_, T> {
        let interrupts_enabled = x86_64::instructions::interrupts::are_enabled();
        if interrupts_enabled {
            x86_64::instructions::interrupts::disable();
        }

        while self
            .state
            .compare_exchange_weak(0, -1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            if crate::arch::x86_64::smp::has_pending_tlb_shootdown() {
                x86_64::instructions::tlb::flush_all();
                crate::arch::x86_64::smp::tlb_shootdown_ack();
            }
            core::hint::spin_loop();
        }

        KRwLockWriteGuard {
            lock: self,
            interrupts_enabled,
        }
    }
}

/// RAII guard for a read lock.
pub struct KRwLockReadGuard<'a, T> {
    lock: &'a KRwLock<T>,
    interrupts_enabled: bool,
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
        if self.interrupts_enabled {
            x86_64::instructions::interrupts::enable();
        }
    }
}

/// RAII guard for a write lock.
pub struct KRwLockWriteGuard<'a, T> {
    lock: &'a KRwLock<T>,
    interrupts_enabled: bool,
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
        if self.interrupts_enabled {
            x86_64::instructions::interrupts::enable();
        }
    }
}
