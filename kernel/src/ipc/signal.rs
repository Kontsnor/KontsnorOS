//! POSIX signal delivery and handling.
//!
//! Signals are asynchronous notifications sent to processes.
//! This module manages signal masks, pending signals, and delivery.

/// Pending signal set for a process.
#[derive(Debug, Clone)]
pub struct SignalSet {
    /// Bitmask of pending signals (bits 1–31).
    bits: u64,
}

impl SignalSet {
    /// Create an empty signal set.
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// Add a signal to the set.
    pub fn add(&mut self, signum: i32) {
        if (1..=31).contains(&signum) {
            self.bits |= 1 << signum;
        }
    }

    /// Remove a signal from the set.
    pub fn remove(&mut self, signum: i32) {
        if (1..=31).contains(&signum) {
            self.bits &= !(1 << signum);
        }
    }

    /// Check if a signal is in the set.
    pub fn contains(&self, signum: i32) -> bool {
        if (1..=31).contains(&signum) {
            self.bits & (1 << signum) != 0
        } else {
            false
        }
    }

    /// Check if any signals are pending.
    pub fn is_empty(&self) -> bool {
        self.bits == 0
    }

    /// Get the lowest-numbered pending signal.
    pub fn first(&self) -> Option<i32> {
        if self.bits == 0 {
            return None;
        }
        Some(self.bits.trailing_zeros() as i32)
    }
}

/// Signal action — what to do when a signal is received.
#[derive(Debug, Clone, Copy)]
pub enum SignalAction {
    /// Use the default action for this signal.
    Default,
    /// Ignore the signal.
    Ignore,
    /// Call a user-space handler function.
    Handler(u64), // function pointer in user space
}

/// Per-process signal state.
#[derive(Debug, Clone)]
pub struct SignalState {
    /// Pending (undelivered) signals.
    pub pending: SignalSet,
    /// Blocked (masked) signals.
    pub blocked: SignalSet,
    /// Signal action table (indexed by signal number).
    pub actions: [SignalAction; 32],
}

impl Default for SignalState {
    fn default() -> Self {
        Self {
            pending: SignalSet::empty(),
            blocked: SignalSet::empty(),
            actions: [SignalAction::Default; 32],
        }
    }
}

impl SignalState {
    /// Queue a signal for delivery.
    pub fn send(&mut self, signum: i32) {
        self.pending.add(signum);
    }

    /// Get the next deliverable signal (pending & not blocked).
    pub fn next_deliverable(&mut self) -> Option<(i32, SignalAction)> {
        // Check each signal, starting from the lowest
        for signum in 1..=31 {
            if self.pending.contains(signum) && !self.blocked.contains(signum) {
                self.pending.remove(signum);
                return Some((signum, self.actions[signum as usize]));
            }
        }
        None
    }
}
