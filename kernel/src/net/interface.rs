//! Network interface management.
//!
//! Manages network interfaces (NICs) and provides the bridge between
//! the protocol stack and the hardware drivers.

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use super::ipv4::Ipv4Addr;
use crate::kprintln;

/// A network interface.
pub struct NetworkInterface {
    /// Interface name (e.g., "eth0", "lo").
    pub name: String,
    /// Interface index.
    pub index: u32,
    /// MAC address.
    pub mac_addr: [u8; 6],
    /// IPv4 address.
    pub ipv4_addr: Ipv4Addr,
    /// Subnet mask.
    pub netmask: Ipv4Addr,
    /// Gateway address.
    pub gateway: Ipv4Addr,
    /// MTU (Maximum Transmission Unit).
    pub mtu: u32,
    /// Whether the interface is up.
    pub is_up: bool,
    /// Received packets counter.
    pub rx_packets: u64,
    /// Transmitted packets counter.
    pub tx_packets: u64,
    /// Received bytes counter.
    pub rx_bytes: u64,
    /// Transmitted bytes counter.
    pub tx_bytes: u64,
    /// Receive errors.
    pub rx_errors: u64,
    /// Transmit errors.
    pub tx_errors: u64,
}

impl NetworkInterface {
    /// Create the loopback interface.
    pub fn loopback() -> Self {
        Self {
            name: String::from("lo"),
            index: 0,
            mac_addr: [0; 6],
            ipv4_addr: Ipv4Addr::LOCALHOST,
            netmask: Ipv4Addr::new(255, 0, 0, 0),
            gateway: Ipv4Addr::UNSPECIFIED,
            mtu: 65535,
            is_up: true,
            rx_packets: 0,
            tx_packets: 0,
            rx_bytes: 0,
            tx_bytes: 0,
            rx_errors: 0,
            tx_errors: 0,
        }
    }

    /// Create a new Ethernet interface.
    pub fn ethernet(name: String, index: u32, mac_addr: [u8; 6]) -> Self {
        Self {
            name,
            index,
            mac_addr,
            ipv4_addr: Ipv4Addr::UNSPECIFIED,
            netmask: Ipv4Addr::UNSPECIFIED,
            gateway: Ipv4Addr::UNSPECIFIED,
            mtu: 1500,
            is_up: false,
            rx_packets: 0,
            tx_packets: 0,
            rx_bytes: 0,
            tx_bytes: 0,
            rx_errors: 0,
            tx_errors: 0,
        }
    }

    /// Configure the interface with an IPv4 address.
    pub fn configure(&mut self, addr: Ipv4Addr, netmask: Ipv4Addr, gateway: Ipv4Addr) {
        self.ipv4_addr = addr;
        self.netmask = netmask;
        self.gateway = gateway;
    }

    /// Bring the interface up.
    pub fn up(&mut self) {
        self.is_up = true;
        kprintln!(
            "[net] Interface {} is UP ({})",
            self.name,
            self.ipv4_addr
        );
    }

    /// Bring the interface down.
    pub fn down(&mut self) {
        self.is_up = false;
        kprintln!("[net] Interface {} is DOWN", self.name);
    }
}

/// Global interface list.
static INTERFACES: Mutex<Option<Vec<NetworkInterface>>> = Mutex::new(None);

/// Initialize the network interface subsystem.
pub fn init() {
    let mut interfaces = Vec::new();

    // Always create the loopback interface
    interfaces.push(NetworkInterface::loopback());

    *INTERFACES.lock() = Some(interfaces);

    super::arp::init();
    super::tcp::init();
    super::udp::init();

    kprintln!("[net] Loopback interface (lo) configured.");
}

/// Register a new network interface.
pub fn register_interface(iface: NetworkInterface) {
    if let Some(ref mut interfaces) = *INTERFACES.lock() {
        kprintln!(
            "[net] Registered interface: {} (MAC: {})",
            iface.name,
            super::ethernet::EthernetHeader::format_mac(&iface.mac_addr)
        );
        interfaces.push(iface);
    }
}

/// Get the number of registered interfaces.
pub fn interface_count() -> usize {
    INTERFACES
        .lock()
        .as_ref()
        .map(|v| v.len())
        .unwrap_or(0)
}
