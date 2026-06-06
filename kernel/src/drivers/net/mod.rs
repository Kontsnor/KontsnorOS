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
