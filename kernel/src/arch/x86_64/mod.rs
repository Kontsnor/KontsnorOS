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

//! x86_64 architecture support for KontsnorOS.
//!
//! This module contains all x86_64-specific code including:
//! - GDT (Global Descriptor Table) setup
//! - IDT (Interrupt Descriptor Table) and interrupt handling
//! - Serial port (UART) driver for early console output
//! - CPU boot sequence

pub mod apic;
pub mod boot;
pub mod gdt;
pub mod interrupts;
pub mod serial;
pub mod smp;
