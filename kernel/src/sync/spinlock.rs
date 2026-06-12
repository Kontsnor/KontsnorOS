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
            self.holding_cpu.load(Ordering::Relaxed) != apic_id,
            "Deadlock: TicketLock recursive re-entrancy detected on CPU {}!",
            apic_id
        );

        // Take a ticket
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);

        // Wait until our ticket is served
        while self.now_serving.load(Ordering::Acquire) != ticket {
            core::hint::spin_loop();
        }

        // Mark the lock as held by this CPU core
        self.holding_cpu.store(apic_id, Ordering::Release);

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

        // If we already hold the lock, fail try_lock to avoid deadlock
        if self.holding_cpu.load(Ordering::Relaxed) == apic_id {
            if interrupts_enabled {
                x86_64::instructions::interrupts::enable();
            }
            return None;
        }

        let current = self.now_serving.load(Ordering::Relaxed);
        let result = self.next_ticket.compare_exchange(
            current,
            current + 1,
            Ordering::Acquire,
            Ordering::Relaxed,
        );

        match result {
            Ok(_) => {
                self.holding_cpu.store(apic_id, Ordering::Release);
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
        // Reset holding CPU
        self.lock.holding_cpu.store(0xFFFFFFFF, Ordering::Release);

        // Advance to the next ticket
        self.lock.now_serving.fetch_add(1, Ordering::Release);

        // Restore the original interrupt state
        if self.interrupts_enabled {
            x86_64::instructions::interrupts::enable();
        }
    }
}
