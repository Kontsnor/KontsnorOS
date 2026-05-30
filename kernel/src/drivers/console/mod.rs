//! Console drivers.

pub mod serial;

/// Initialize console drivers.
pub fn init() {
    serial::init();
}
