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

//! Custom in-kernel test suite execution and test runner infrastructure.

use crate::kprintln;
use core::panic::PanicInfo;

#[path = "tests/mod.rs"]
pub mod tests;

/// Trait implemented by test functions to enable declarative naming and execution.
pub trait Testable {
    fn run(&self);
    fn name(&self) -> &'static str;
}

impl<T: Fn()> Testable for T {
    fn run(&self) {
        self();
    }
    fn name(&self) -> &'static str {
        core::any::type_name::<T>()
    }
}

/// QEMU exit status codes mapping to the isa-debug-exit device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

/// Shuts down QEMU with the specified status code using the isa-debug-exit device.
pub fn exit_qemu(exit_code: QemuExitCode) -> ! {
    use x86_64::instructions::port::Port;
    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
    loop {
        x86_64::instructions::hlt();
    }
}

/// Custom test runner executing a slice of test cases with RDTSC execution timing.
pub fn test_runner(tests: &[&dyn Testable]) {
    kprintln!("Running {} tests", tests.len());
    let tsc_khz = 2_000_000u64; // Nominally 2.0 GHz for timing calculations

    for test in tests {
        let name = test.name();
        let short_name = name.rsplit("::").next().unwrap_or(name);

        let start_tsc = unsafe { core::arch::x86_64::_rdtsc() };
        test.run();
        let end_tsc = unsafe { core::arch::x86_64::_rdtsc() };
        let elapsed_cycles = end_tsc.saturating_sub(start_tsc);
        let elapsed_ms_x100 = (elapsed_cycles * 100) / (tsc_khz * 1000);
        let whole_ms = elapsed_ms_x100 / 100;
        let frac_ms = elapsed_ms_x100 % 100;

        kprintln!("[ok] {} ({}.{:02} ms)", short_name, whole_ms, frac_ms);
    }
    kprintln!("All tests passed!");
    exit_qemu(QemuExitCode::Success);
}

/// Test mode panic handler.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    x86_64::instructions::interrupts::disable();
    kprintln!();
    kprintln!("!!! TEST PANIC !!!");
    kprintln!("==================");
    if let Some(location) = info.location() {
        kprintln!(
            "  Location: {}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        );
    }
    if let Some(message) = info.message().as_str() {
        kprintln!("  Message: {}", message);
    } else {
        kprintln!("  Message: {}", info.message());
    }
    kprintln!("==================");
    kprintln!("Test failed.");
    kprintln!();
    exit_qemu(QemuExitCode::Failed);
}
