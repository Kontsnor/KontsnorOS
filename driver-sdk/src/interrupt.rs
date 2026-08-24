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
