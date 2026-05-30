# KontsnorOS Driver Development Guide

This guide covers everything you need to know to write a hardware driver
for KontsnorOS.

## Overview

KontsnorOS uses a **trait-based driver model** where drivers implement
well-defined Rust traits for their device category:

| Device Type | Trait | Examples |
|-------------|-------|----------|
| Character | `CharDevice` | Serial ports, terminals, keyboards |
| Block | `BlockDevice` | Hard drives, SSDs, NVMe |
| Network | `NetDevice` | Ethernet, WiFi adapters |
| GPU | `GpuDevice` | NVIDIA, AMD, Intel GPUs |

## Getting Started

### 1. Set Up Your Driver Project

```toml
# Cargo.toml
[package]
name = "my-driver"
version = "0.1.0"
edition = "2024"

[dependencies]
kontsnor-driver-sdk = { path = "../driver-sdk" }
```

### 2. Implement the Device Trait

```rust
#![no_std]

use kontsnor_driver_sdk::*;

pub struct MyNetDriver {
    base_addr: u64,
}

impl NetDevice for MyNetDriver {
    fn send(&self, data: &[u8]) -> Result<(), DriverError> {
        // Program your hardware to transmit the packet
        Ok(())
    }

    fn recv(&self, buf: &mut [u8]) -> Result<usize, DriverError> {
        // Read received packet from hardware buffer
        Ok(0)
    }

    fn mac_address(&self) -> [u8; 6] {
        [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01]
    }

    fn info(&self) -> DriverInfo {
        DriverInfo {
            name: "my-net-driver".into(),
            version: "1.0.0".into(),
            author: "My Company".into(),
            license: "MIT".into(),
            description: "My custom network driver".into(),
        }
    }
}
```

### 3. Register Your Driver

```rust
pub fn init() {
    let driver = MyNetDriver { base_addr: 0x1000 };
    kontsnor_driver_sdk::register_driver(driver.info());
}
```

## GPU Driver Development

For GPU drivers, implement the `GpuDevice` trait:

### Required Methods

- `init_hw()` — Initialize GPU hardware, detect display outputs
- `get_display_info()` — Return connected displays and supported modes
- `set_mode()` — Configure display resolution and refresh rate
- `get_framebuffer()` — Return framebuffer address for display output

### Optional Methods (Hardware Acceleration)

- `submit_commands()` — Submit GPU command buffers for 3D rendering
- `wait_fence()` — Wait for GPU to finish processing commands
- `alloc_vram()` — Allocate GPU memory (VRAM)
- `free_vram()` — Free GPU memory

### Example: NVIDIA GPU Driver Skeleton

```rust
pub struct NvidiaGpu {
    pci_device: PciDeviceId,
    mmio: MmioRegion,
}

impl GpuDevice for NvidiaGpu {
    fn init_hw(&self) -> Result<(), DriverError> {
        // 1. Read PCI BARs
        // 2. Map MMIO registers
        // 3. Initialize GPU firmware (GSP)
        // 4. Detect connected displays
        Ok(())
    }

    fn get_display_info(&self) -> Vec<DisplayInfo> {
        // Query display connectors (HDMI, DP, etc.)
        vec![DisplayInfo {
            id: 0,
            name: "HDMI-1".into(),
            connected: true,
            modes: vec![DisplayMode {
                width: 1920,
                height: 1080,
                refresh_rate: 60,
                bpp: 32,
            }],
        }]
    }

    // ... other implementations
}
```

## Hardware Access

### MMIO (Memory-Mapped I/O)

```rust
use kontsnor_driver_sdk::MmioRegion;

let mmio = MmioRegion { base: 0xFE000000, size: 0x10000 };

// Read a register
let value = unsafe { mmio.read_u32(0x100) };

// Write a register
unsafe { mmio.write_u32(0x100, 0x1234) };
```

### DMA Buffers

```rust
use kontsnor_driver_sdk::DmaBuffer;

// Allocate a DMA buffer for hardware data transfer
let dma_buf = DmaBuffer {
    virt_addr: 0x1000,
    phys_addr: 0x1000,
    size: 4096,
};

// The physical address is programmed into the hardware DMA engine
// The driver reads/writes through the virtual address
```

## Licensing

The driver SDK is dual-licensed under **MIT and Apache-2.0**. You are free to
write proprietary drivers that use the public SDK API. We encourage open-source
drivers, but it is not required.

## Questions?

Open an issue on GitHub or join our community discussions.
