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

//! Network drivers for KontsnorOS.

pub mod e1000;

/// Initialize network drivers by probing the PCI bus.
pub fn init() {
    let devices = crate::drivers::bus::pci::find_device(0x8086, 0x100e);
    if !devices.is_empty() {
        let dev = &devices[0];
        unsafe {
            e1000::init(dev.bus, dev.device, dev.function);
        }
    } else {
        crate::kprintln!("[net-drivers] No Intel e1000 network card found on PCI bus.");
    }
}
