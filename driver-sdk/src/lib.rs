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

//! # KontsnorOS Driver SDK
//!
//! The official SDK for developing hardware drivers for KontsnorOS.
//!
//! ## Quick Start
//!
//! To write a driver for KontsnorOS:
//!
//! 1. Add `kontsnor-driver-sdk` as a dependency
//! 2. Implement the appropriate device trait (`CharDevice`, `BlockDevice`,
//!    `NetDevice`, or `GpuDevice`)
//! 3. Register your driver with the kernel using `register_driver()`
//!
//! ## Example: Simple Character Device
//!
//! ```rust,no_run
//! use kontsnor_driver_sdk::*;
//!
//! pub struct MyDevice;
//!
//! impl CharDevice for MyDevice {
//!     fn read(&self, buf: &mut [u8]) -> Result<usize, DriverError> {
//!         // Read from your hardware
//!         Ok(0)
//!     }
//!
//!     fn write(&self, data: &[u8]) -> Result<usize, DriverError> {
//!         // Write to your hardware
//!         Ok(data.len())
//!     }
//!
//!     fn info(&self) -> DriverInfo {
//!         DriverInfo {
//!             name: "my-device".into(),
//!             version: "1.0.0".into(),
//!             author: "Your Company".into(),
//!             license: "GPL-3.0-only".into(),
//!             description: "My custom device driver".into(),
//!         }
//!     }
//! }
//! ```
//!
//! ## GPU Driver Development
//!
//! For GPU drivers (targeting NVIDIA, AMD, or other hardware), implement
//! the `GpuDevice` trait. This provides a modern, Rust-native interface
//! for:
//!
//! - Display mode setting and framebuffer access
//! - Command buffer submission (for hardware acceleration)
//! - GPU memory (VRAM) management
//! - Fence-based synchronization
//!
//! The SDK is designed to be familiar to developers who have worked with
//! Linux's DRM/KMS subsystem, but with the safety guarantees of Rust.
//!
//! ## License
//!
//! This SDK is licensed under the GNU General Public License v3.0 (GPLv3 only).

#![no_std]
#![warn(missing_docs)]

extern crate alloc;

mod alloc_api;
mod bus;
mod dma;
mod interrupt;
mod io;
mod traits;

pub use alloc_api::*;
pub use bus::*;
pub use dma::*;
pub use interrupt::*;
pub use io::*;
pub use traits::*;
