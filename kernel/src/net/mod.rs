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

//! Network stack for KontsnorOS.
//!
//! Implements the core networking protocols from the ground up:
//!
//! ```text
//!  ┌───────────────────────────────┐
//!  │    Socket Layer (BSD API)     │  ← User-facing interface
//!  ├───────────────────────────────┤
//!  │    TCP / UDP / ICMP           │  ← Transport layer
//!  ├───────────────────────────────┤
//!  │    IPv4 / IPv6                │  ← Network layer
//!  ├───────────────────────────────┤
//!  │    ARP                        │  ← Link-layer resolution
//!  ├───────────────────────────────┤
//!  │    Ethernet                   │  ← Data link layer
//!  ├───────────────────────────────┤
//!  │    Network Device (NetDevice) │  ← Hardware abstraction
//!  └───────────────────────────────┘
//! ```

pub mod arp;
pub mod ethernet;
pub mod icmp;
pub mod interface;
pub mod ipv4;
pub mod socket;
pub mod tcp;
pub mod udp;

use crate::kprintln;

/// Initialize the network stack.
pub fn init() {
    interface::init();
    kprintln!("[net] Network stack initialized.");
}
