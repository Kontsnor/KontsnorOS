//! Platform device bus.
//!
//! For devices that are not on a discoverable bus (like PCI or USB),
//! they are registered as "platform devices" with known I/O addresses
//! and IRQs.

/// Initialize the platform bus.
use crate::kprintln;
pub fn init() {
    kprintln!("[platform] Platform bus initialized.");
}
