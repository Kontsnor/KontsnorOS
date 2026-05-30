//! PID (Process ID) allocator.
//!
//! Provides unique, monotonically increasing process identifiers.
//! PID 0 is reserved for the kernel idle task.
//! PID 1 is reserved for the init process.

use core::sync::atomic::{AtomicU64, Ordering};
use crate::kprintln;

/// The next PID to allocate.
/// Starts at 2 (0 = idle, 1 = init).
static NEXT_PID: AtomicU64 = AtomicU64::new(2);

/// A unique process identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pid(u64);

impl Pid {
    /// The kernel idle task PID.
    pub const IDLE: Pid = Pid(0);

    /// The init process PID.
    pub const INIT: Pid = Pid(1);

    /// Create a PID from a raw value.
    pub const fn from_raw(val: u64) -> Self {
        Self(val)
    }

    /// Get the raw PID value.
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl core::fmt::Display for Pid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Initialize the PID allocator.
pub fn init() {
    // Already initialized via static, but this is a hook for future work
    kprintln!("[pid] PID allocator initialized (next PID: 2).");
}

/// Allocate a new unique PID.
///
/// PIDs are never reused (until we implement PID recycling).
pub fn allocate() -> Pid {
    Pid(NEXT_PID.fetch_add(1, Ordering::Relaxed))
}
