//! Interrupt (IRQ) registration for drivers.

/// An IRQ handler registration.
#[derive(Debug)]
pub struct IrqRegistration {
    /// The IRQ number.
    pub irq: u32,
    /// Whether this is a shared interrupt line.
    pub shared: bool,
}

/// IRQ handler return values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqReturn {
    /// The interrupt was handled by this driver.
    Handled,
    /// The interrupt was not for this driver (shared IRQ).
    NotHandled,
}
