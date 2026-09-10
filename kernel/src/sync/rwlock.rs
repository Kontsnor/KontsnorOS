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
//!
//! ## Writer Priority / Starvation Prevention
//!
//! A `writer_pending` flag is set as soon as a writer begins waiting. While
//! the flag is set, new reader acquisitions spin without incrementing the
//! reader count. This gives writers bounded wait time regardless of how many
//! concurrent readers arrive, preventing the classic reader-starvation-of-writers
//! problem common in naive shared-count rwlocks.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};

/// A reader-writer lock.
///
/// - Positive count = number of active readers
/// - Zero = unlocked
/// - -1 = locked by a writer
///
/// A `writer_pending` flag prevents new readers from acquiring the lock
/// while a writer is waiting, eliminating writer starvation under heavy
/// concurrent read workloads (e.g., the global `TASKS` table under SMP).
pub struct KRwLock<T> {
    /// Lock state: >0 = readers, 0 = free, -1 = writer.
    state: AtomicI64,
    /// Set to `true` while a writer is waiting or holding the lock.
    /// New readers must spin when this is set, giving the writer priority.
    writer_pending: AtomicBool,
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
            writer_pending: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    /// Acquire a read lock.
    ///
    /// Spins while a writer is pending or active, then atomically
    /// increments the reader count.
    pub fn read(&self) -> KRwLockReadGuard<'_, T> {
        let interrupts_enabled = x86_64::instructions::interrupts::are_enabled();
        if interrupts_enabled {
            x86_64::instructions::interrupts::disable();
        }

        loop {
            // Respect writer priority: spin while a writer is pending or active.
            if self.writer_pending.load(Ordering::Acquire) {
                if crate::arch::x86_64::smp::has_pending_tlb_shootdown() {
                    x86_64::instructions::tlb::flush_all();
                    crate::arch::x86_64::smp::tlb_shootdown_ack();
                }
                core::hint::spin_loop();
                continue;
            }

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
    ///
    /// Sets `writer_pending` before spinning so that new readers back off,
    /// then waits until the reader count drops to zero before acquiring.
    pub fn write(&self) -> KRwLockWriteGuard<'_, T> {
        let interrupts_enabled = x86_64::instructions::interrupts::are_enabled();
        if interrupts_enabled {
            x86_64::instructions::interrupts::disable();
        }

        // Signal to incoming readers that a writer wants the lock.
        // This prevents starvation by blocking new readers from entering.
        self.writer_pending.store(true, Ordering::Release);

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

        // Lock acquired; the guard's Drop will clear writer_pending.
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
        // Release the write lock and clear writer_pending so readers can proceed.
        self.lock.state.store(0, Ordering::Release);
        self.lock.writer_pending.store(false, Ordering::Release);
        if self.interrupts_enabled {
            x86_64::instructions::interrupts::enable();
        }
    }
}
