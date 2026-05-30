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
pub mod tcp;
pub mod udp;

use crate::kprintln;

/// Initialize the network stack.
pub fn init() {
    interface::init();
    kprintln!("[net] Network stack initialized.");
}
