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

//! Ticket spinlock implementation.
//!
//! A ticket spinlock provides fairness (FIFO ordering) compared to a
//! simple test-and-set spinlock. Each thread takes a "ticket" and waits
//! until its number is called.
//!
//! Spinlocks automatically disable interrupts while held to prevent
//! deadlocks from interrupt handlers trying to acquire the same lock.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// A ticket-based spinlock.
///
/// This lock disables interrupts while held to prevent deadlocks.
pub struct TicketLock<T> {
    /// Next ticket number to assign.
    next_ticket: AtomicU64,
    /// Current serving number.
    now_serving: AtomicU64,
    /// CPU core currently holding the lock (0xFFFFFFFF if free).
    holding_cpu: AtomicU32,
    /// The protected data.
    data: UnsafeCell<T>,
}

// SAFETY: TicketLock provides mutual exclusion through atomic operations.
unsafe impl<T: Send> Send for TicketLock<T> {}
unsafe impl<T: Send> Sync for TicketLock<T> {}

impl<T> TicketLock<T> {
    /// Create a new ticket lock.
    pub const fn new(data: T) -> Self {
        Self {
            next_ticket: AtomicU64::new(0),
            now_serving: AtomicU64::new(0),
            holding_cpu: AtomicU32::new(0xFFFFFFFF),
            data: UnsafeCell::new(data),
        }
    }

    /// Retrieve the current tickets and holding CPU for debugging.
    pub fn debug_info(&self) -> (u64, u64, u32) {
        (
            self.next_ticket.load(Ordering::Relaxed),
            self.now_serving.load(Ordering::Relaxed),
            self.holding_cpu.load(Ordering::Relaxed),
        )
    }

    /// Retrieve the CPU ID currently holding the lock.
    pub fn holding_cpu_id(&self) -> u32 {
        self.holding_cpu.load(Ordering::Relaxed)
    }

    /// Acquire the lock, returning a guard that releases it on drop.
    ///
    /// This will busy-wait until the lock is available. Interrupts
    /// are disabled while the lock is held.
    pub fn lock(&self) -> TicketLockGuard<'_, T> {
        // Query if interrupts are currently enabled on this CPU core
        let interrupts_enabled = x86_64::instructions::interrupts::are_enabled();

        // Disable interrupts to prevent deadlocks from interrupt handlers
        if interrupts_enabled {
            x86_64::instructions::interrupts::disable();
        }

        // Get the active Local APIC ID
        let apic_id = crate::arch::x86_64::smp::current_lapic_id() as u32;

        // Assert that the current CPU core doesn't already hold the lock (prevent recursive deadlocks)
        assert!(
            self.holding_cpu.load(Ordering::SeqCst) != apic_id,
            "Deadlock: TicketLock at {:p} recursive re-entrancy detected on CPU {}!",
            self,
            apic_id
        );

        // Take a ticket
        let ticket = self.next_ticket.fetch_add(1, Ordering::SeqCst);

        // Wait until our ticket is served
        while self.now_serving.load(Ordering::SeqCst) != ticket {
            if crate::arch::x86_64::smp::has_pending_tlb_shootdown() {
                x86_64::instructions::tlb::flush_all();
                crate::arch::x86_64::smp::tlb_shootdown_ack();
            }
            core::hint::spin_loop();
        }

        // Mark the lock as held by this CPU core
        self.holding_cpu.store(apic_id, Ordering::SeqCst);

        TicketLockGuard {
            lock: self,
            interrupts_enabled,
        }
    }

    /// Try to acquire the lock without blocking.
    pub fn try_lock(&self) -> Option<TicketLockGuard<'_, T>> {
        let interrupts_enabled = x86_64::instructions::interrupts::are_enabled();
        if interrupts_enabled {
            x86_64::instructions::interrupts::disable();
        }

        let apic_id = crate::arch::x86_64::smp::current_lapic_id() as u32;

        // If the lock is currently held, fail try_lock immediately
        if self.holding_cpu.load(Ordering::SeqCst) != 0xFFFFFFFF {
            if interrupts_enabled {
                x86_64::instructions::interrupts::enable();
            }
            return None;
        }

        let current = self.now_serving.load(Ordering::SeqCst);
        let result = self.next_ticket.compare_exchange(
            current,
            current + 1,
            Ordering::SeqCst,
            Ordering::Relaxed,
        );

        match result {
            Ok(_) => {
                self.holding_cpu.store(apic_id, Ordering::SeqCst);
                Some(TicketLockGuard {
                    lock: self,
                    interrupts_enabled,
                })
            }
            Err(_) => {
                if interrupts_enabled {
                    x86_64::instructions::interrupts::enable();
                }
                None
            }
        }
    }

    /// Force unlock the ticket lock without an RAII guard.
    ///
    /// # Safety
    /// This is unsafe because it bypasses normal RAII lock guard guarantees.
    pub unsafe fn force_unlock(&self) {
        let prev = self.holding_cpu.swap(0xFFFFFFFF, Ordering::SeqCst);
        if prev != 0xFFFFFFFF {
            self.now_serving.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Get a mutable reference to the underlying data without locking.
    ///
    /// # Safety
    /// This is unsafe because it bypasses lock guarantees. The caller must ensure
    /// that no other CPU core is concurrently accessing the data.
    pub unsafe fn get_mut_unchecked(&self) -> &mut T {
        unsafe { &mut *self.data.get() }
    }
}

/// RAII guard for a ticket lock.
pub struct TicketLockGuard<'a, T> {
    lock: &'a TicketLock<T>,
    interrupts_enabled: bool,
}

impl<T> Deref for TicketLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: We hold the lock.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for TicketLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: We hold the lock exclusively.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for TicketLockGuard<'_, T> {
    fn drop(&mut self) {
        // Reset holding CPU and only advance now_serving if the lock was actively held
        let prev = self.lock.holding_cpu.swap(0xFFFFFFFF, Ordering::SeqCst);
        if prev != 0xFFFFFFFF {
            self.lock.now_serving.fetch_add(1, Ordering::SeqCst);
        }

        // Restore the original interrupt state
        if self.interrupts_enabled {
            x86_64::instructions::interrupts::enable();
        }
    }
}
