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

//! Bochs VBE Display Adapter Driver and Graphics Console.
//!
//! Exposes a GPU driver that switches the Bochs/QEMU VGA card to a high-resolution
//! 1024x768 32-bit linear framebuffer mode, renders a boot splash screen,
//! and provides a standard ASCII font graphics console with ANSI escape parsing.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

use crate::drivers::gpu::framebuffer::Color;
use crate::drivers::traits::{
    DisplayInfo, DisplayMode, DriverError, DriverInfo, FramebufferInfo, GpuDevice,
};

// Bochs VBE Register Indices
const VBE_DISPI_INDEX_ID: u16 = 0;
const VBE_DISPI_INDEX_XRES: u16 = 1;
const VBE_DISPI_INDEX_YRES: u16 = 2;
const VBE_DISPI_INDEX_BPP: u16 = 3;
const VBE_DISPI_INDEX_ENABLE: u16 = 4;
const VBE_DISPI_INDEX_BANK: u16 = 5;
const VBE_DISPI_INDEX_VIRT_WIDTH: u16 = 6;
const VBE_DISPI_INDEX_VIRT_HEIGHT: u16 = 7;
const VBE_DISPI_INDEX_X_OFFSET: u16 = 8;
const VBE_DISPI_INDEX_Y_OFFSET: u16 = 9;

// Bochs VBE Register Values
const VBE_DISPI_DISABLED: u16 = 0x00;
const VBE_DISPI_ENABLED: u16 = 0x01;
const VBE_DISPI_LFB_ENABLED: u16 = 0x40;

// VBE I/O Ports
const VBE_DISPI_IOPORT_INDEX: u16 = 0x01CE;
const VBE_DISPI_IOPORT_DATA: u16 = 0x01CF;

/// Write to a VBE register.
fn write_vbe(index: u16, val: u16) {
    let mut index_port = Port::<u16>::new(VBE_DISPI_IOPORT_INDEX);
    let mut data_port = Port::<u16>::new(VBE_DISPI_IOPORT_DATA);
    // SAFETY: Writing to Bochs VBE configuration ports is standard register access for setting display resolutions and modes.
    unsafe {
        index_port.write(index);
        data_port.write(val);
    }
}

/// Read from a VBE register.
fn read_vbe(index: u16) -> u16 {
    let mut index_port = Port::<u16>::new(VBE_DISPI_IOPORT_INDEX);
    let mut data_port = Port::<u16>::new(VBE_DISPI_IOPORT_DATA);
    // SAFETY: Reading from Bochs VBE configuration ports is standard register access for querying display state.
    unsafe {
        index_port.write(index);
        data_port.read()
    }
}

/// The low-level Bochs VBE GPU state.
pub struct BochsGpu {
    pub lfb_phys: u64,
    pub lfb_virt: u64,
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
    pub size: u64,
    pub backbuffer: Mutex<Vec<u32>>,
}

impl BochsGpu {
    /// Copy the backbuffer to the physical linear frame buffer.
    pub fn blit(&self) {
        // SAFETY: We copy the pixels from our heap-allocated backbuffer (with length matching the active screen dimensions)
        // to the mapped graphics framebuffer virtual address space `self.lfb_virt`.
        unsafe {
            let dest = self.lfb_virt as *mut u32;
            let back = self.backbuffer.lock();
            core::ptr::copy_nonoverlapping(
                back.as_ptr(),
                dest,
                (self.width * self.height) as usize,
            );
        }
    }
}

/// High-level GpuDevice wrapper.
pub struct BochsGpuDevice {
    pub gpu: ArcMutexGpu,
}

/// Arc/Mutex wrapper around BochsGpu for sharing.
#[derive(Clone)]
pub struct ArcMutexGpu(pub alloc::sync::Arc<BochsGpu>);

impl GpuDevice for BochsGpuDevice {
    fn init_hw(&self) -> Result<(), DriverError> {
        Ok(())
    }

    fn get_display_info(&self) -> Vec<DisplayInfo> {
        vec![DisplayInfo {
            id: 0,
            name: String::from("VGA-0"),
            connected: true,
            modes: vec![DisplayMode {
                width: self.gpu.0.width,
                height: self.gpu.0.height,
                refresh_rate: 60,
                bpp: self.gpu.0.bpp,
            }],
        }]
    }

    fn set_mode(&self, _display: u32, mode: &DisplayMode) -> Result<(), DriverError> {
        write_vbe(VBE_DISPI_INDEX_ENABLE, VBE_DISPI_DISABLED);
        write_vbe(VBE_DISPI_INDEX_XRES, mode.width as u16);
        write_vbe(VBE_DISPI_INDEX_YRES, mode.height as u16);
        write_vbe(VBE_DISPI_INDEX_BPP, mode.bpp as u16);
        write_vbe(
            VBE_DISPI_INDEX_ENABLE,
            VBE_DISPI_ENABLED | VBE_DISPI_LFB_ENABLED,
        );
        Ok(())
    }

    fn get_framebuffer(&self, _display: u32) -> Result<FramebufferInfo, DriverError> {
        Ok(FramebufferInfo {
            phys_addr: self.gpu.0.lfb_phys,
            size: self.gpu.0.size,
            stride: self.gpu.0.width * 4,
            width: self.gpu.0.width,
            height: self.gpu.0.height,
            bpp: self.gpu.0.bpp,
        })
    }

    fn info(&self) -> DriverInfo {
        DriverInfo {
            name: String::from("bochs-gpu"),
            version: String::from("0.1.0"),
            author: String::from("KontsnorOS Core Devs"),
            license: String::from("GPL-3.0-only"),
            description: String::from("Bochs/QEMU VBE PCI Display Driver"),
        }
    }
}

/// ANSI parsing state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnsiState {
    Normal,
    Esc,
    Bracket {
        params: Vec<u32>,
        current_param: Option<u32>,
    },
}

/// The Graphics Console that acts as a text terminal emulator.
pub struct GraphicsConsole {
    pub gpu: alloc::sync::Arc<BochsGpu>,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub fg_color: Color,
    pub bg_color: Color,
    pub ansi_state: AnsiState,
}

impl GraphicsConsole {
    /// Clear the backbuffer to a solid color and reset cursor.
    pub fn clear(&mut self, color: Color) {
        let val = color.to_argb32();
        self.gpu.backbuffer.lock().fill(val);
        self.cursor_x = 0;
        self.cursor_y = 0;
    }

    /// Draw a character glyph onto the backbuffer at arbitrary pixel coordinates (x, y).
    pub fn draw_char(&self, x: usize, y: usize, c: u8, fg_color: Color, bg_color: Color) {
        let font_data = include_bytes!("font_8x16.bin");
        let char_offset = (c as usize) * 16;
        let mut back = self.gpu.backbuffer.lock();
        let width = self.gpu.width as usize;
        let height = self.gpu.height as usize;

        for y_offset in 0..16 {
            let pixel_y = y + y_offset;
            if pixel_y >= height {
                continue;
            }
            let row_byte = font_data[char_offset + y_offset];

            for x_offset in 0..8 {
                let pixel_x = x + x_offset;
                if pixel_x >= width {
                    continue;
                }
                let bit = (row_byte >> (7 - x_offset)) & 1;
                let color = if bit == 1 { fg_color } else { bg_color };
                let index = pixel_y * width + pixel_x;
                if index < back.len() {
                    back[index] = color.to_argb32();
                }
            }
        }
    }

    /// Render a string at arbitrary pixel coordinates (x, y).
    pub fn draw_string(&self, x: usize, y: usize, text: &str, fg_color: Color, bg_color: Color) {
        let mut curr_x = x;
        for &byte in text.as_bytes() {
            self.draw_char(curr_x, y, byte, fg_color, bg_color);
            curr_x += 8;
        }
    }

    /// Draw a character glyph onto the backbuffer at character coordinates (col, row).
    pub fn draw_char_at(&self, col: usize, row: usize, c: u8) {
        let cols = self.gpu.width as usize / 8;
        let rows = self.gpu.height as usize / 16;
        if col >= cols || row >= rows {
            return;
        }
        self.draw_char(col * 8, row * 16, c, self.fg_color, self.bg_color);
    }

    /// Advance cursor and handle scrolling.
    pub fn newline(&mut self) {
        self.cursor_x = 0;
        let rows = self.gpu.height as usize / 16;
        if self.cursor_y < rows - 1 {
            self.cursor_y += 1;
        } else {
            self.scroll_up();
        }
    }

    /// Scroll console up by 1 line (16 pixels).
    pub fn scroll_up(&mut self) {
        let mut back = self.gpu.backbuffer.lock();
        let width = self.gpu.width as usize;
        let height = self.gpu.height as usize;
        let lines_to_copy = height - 16;
        // SAFETY: We copy the pixels within the bounds of the backbuffer size (width * height).
        // The pointer additions and copy are fully checked against the pointer boundaries and bounds.
        unsafe {
            let ptr = back.as_mut_ptr();
            core::ptr::copy(ptr.add(width * 16), ptr, width * lines_to_copy);

            let bg_val = self.bg_color.to_argb32();
            for i in (width * lines_to_copy)..(width * height) {
                if i < back.len() {
                    *ptr.add(i) = bg_val;
                }
            }
        }
    }

    /// Render a scaled string (for boot logo/splash).
    pub fn draw_string_scaled(&self, x: usize, y: usize, s: &str, scale: usize, color: Color) {
        let font_data = include_bytes!("font_8x16.bin");
        let mut curr_x = x;
        let mut back = self.gpu.backbuffer.lock();

        for &c in s.as_bytes() {
            let char_offset = (c as usize) * 16;

            for y_offset in 0..16 {
                let row_byte = font_data[char_offset + y_offset];

                for x_offset in 0..8 {
                    let bit = (row_byte >> (7 - x_offset)) & 1;
                    if bit == 1 {
                        for sy in 0..scale {
                            for sx in 0..scale {
                                let px = curr_x + x_offset * scale + sx;
                                let py = y + y_offset * scale + sy;
                                if px < 1024 && py < 768 {
                                    back[py * 1024 + px] = color.to_argb32();
                                }
                            }
                        }
                    }
                }
            }
            curr_x += 8 * scale;
        }
    }

    /// Draw a horizontal progress bar.
    pub fn draw_progress_bar(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        progress_percent: usize,
        color: Color,
    ) {
        let mut back = self.gpu.backbuffer.lock();
        let fill_w = (w * progress_percent) / 100;

        for dy in 0..h {
            for dx in 0..w {
                let px = x + dx;
                let py = y + dy;
                if px < 1024 && py < 768 {
                    let pixel_color = if dx < fill_w {
                        color.to_argb32()
                    } else {
                        Color::rgb(50, 70, 100).to_argb32()
                    };
                    back[py * 1024 + px] = pixel_color;
                }
            }
        }
    }

    /// Render a beautiful boot splash screen.
    pub fn draw_splash(&mut self) {
        self.clear(Color::rgb(10, 20, 35));

        // Horizontal borders
        {
            let mut back = self.gpu.backbuffer.lock();
            let blue_val = Color::BRAND_BLUE.to_argb32();
            for y in 0..5 {
                for x in 0..1024 {
                    back[y * 1024 + x] = blue_val;
                    back[(763 + y) * 1024 + x] = blue_val;
                }
            }
        }

        // Draw system title and description
        self.draw_string_scaled(200, 250, "KontsnorOS", 4, Color::BRAND_ACCENT);
        self.draw_string_scaled(
            200,
            330,
            "A Unix-Compatible Hybrid Kernel in Rust",
            1,
            Color::WHITE,
        );

        // Draw loading bar
        self.draw_progress_bar(200, 380, 624, 6, 60, Color::BRAND_BLUE);

        // Subtext
        self.draw_string_scaled(
            200,
            410,
            "Initializing hardware subsystems...",
            1,
            Color::rgb(150, 170, 190),
        );
    }

    /// Write a character byte, parsing ANSI escapes.
    pub fn write_char(&mut self, c: u8) {
        let cols = self.gpu.width as usize / 8;
        match self.ansi_state.clone() {
            AnsiState::Normal => {
                if c == b'\x1b' {
                    self.ansi_state = AnsiState::Esc;
                } else if c == b'\n' {
                    self.newline();
                } else if c == b'\r' {
                    self.cursor_x = 0;
                } else if c == b'\t' {
                    let next_tab = (self.cursor_x + 8) & !7;
                    while self.cursor_x < next_tab && self.cursor_x < cols {
                        self.draw_char_at(self.cursor_x, self.cursor_y, b' ');
                        self.cursor_x += 1;
                    }
                    if self.cursor_x >= cols {
                        self.newline();
                    }
                } else if c == 0x08 || c == 0x7F {
                    if self.cursor_x > 0 {
                        self.cursor_x -= 1;
                        self.draw_char_at(self.cursor_x, self.cursor_y, b' ');
                    }
                } else {
                    self.draw_char_at(self.cursor_x, self.cursor_y, c);
                    self.cursor_x += 1;
                    if self.cursor_x >= cols {
                        self.newline();
                    }
                }
            }
            AnsiState::Esc => {
                if c == b'[' {
                    self.ansi_state = AnsiState::Bracket {
                        params: Vec::new(),
                        current_param: None,
                    };
                } else {
                    self.ansi_state = AnsiState::Normal;
                }
            }
            AnsiState::Bracket {
                mut params,
                current_param,
            } => {
                if c >= b'0' && c <= b'9' {
                    let digit = (c - b'0') as u32;
                    let val = current_param.unwrap_or(0) * 10 + digit;
                    self.ansi_state = AnsiState::Bracket {
                        params,
                        current_param: Some(val),
                    };
                } else if c == b';' {
                    params.push(current_param.unwrap_or(0));
                    self.ansi_state = AnsiState::Bracket {
                        params,
                        current_param: None,
                    };
                } else if c == b'm' {
                    let mut final_params = params;
                    final_params.push(current_param.unwrap_or(0));
                    if final_params.is_empty() {
                        final_params.push(0);
                    }
                    self.apply_ansi_colors(&final_params);
                    self.ansi_state = AnsiState::Normal;
                } else {
                    self.ansi_state = AnsiState::Normal;
                }
            }
        }
    }

    /// Process ANSI parameters to set fg/bg colors.
    fn apply_ansi_colors(&mut self, params: &[u32]) {
        let mut bold = false;
        for &param in params {
            match param {
                0 => {
                    self.fg_color = Color::WHITE;
                    self.bg_color = Color::BLACK;
                    bold = false;
                }
                1 => {
                    bold = true;
                }
                30 => self.fg_color = Color::rgb(0, 0, 0), // Black
                31 => self.fg_color = Color::rgb(205, 0, 0), // Red
                32 => self.fg_color = Color::rgb(0, 205, 0), // Green
                33 => self.fg_color = Color::rgb(205, 205, 0), // Yellow
                34 => self.fg_color = Color::rgb(0, 0, 238), // Blue
                35 => self.fg_color = Color::rgb(205, 0, 205), // Magenta
                36 => self.fg_color = Color::rgb(0, 205, 205), // Cyan
                37 => self.fg_color = Color::rgb(229, 229, 229), // White
                39 => self.fg_color = Color::WHITE,
                40 => self.bg_color = Color::rgb(0, 0, 0),
                41 => self.bg_color = Color::rgb(205, 0, 0),
                42 => self.bg_color = Color::rgb(0, 205, 0),
                43 => self.bg_color = Color::rgb(205, 205, 0),
                44 => self.bg_color = Color::rgb(0, 0, 238),
                45 => self.bg_color = Color::rgb(205, 0, 205),
                46 => self.bg_color = Color::rgb(0, 205, 205),
                47 => self.bg_color = Color::rgb(229, 229, 229),
                49 => self.bg_color = Color::BLACK,
                90 => self.fg_color = Color::rgb(127, 127, 127), // Bright Black
                91 => self.fg_color = Color::rgb(255, 0, 0),     // Bright Red
                92 => self.fg_color = Color::rgb(0, 255, 0),     // Bright Green
                93 => self.fg_color = Color::rgb(255, 255, 0),   // Bright Yellow
                94 => self.fg_color = Color::rgb(92, 92, 255),   // Bright Blue
                95 => self.fg_color = Color::rgb(255, 0, 255),   // Bright Magenta
                96 => self.fg_color = Color::rgb(0, 255, 255),   // Bright Cyan
                97 => self.fg_color = Color::rgb(255, 255, 255), // Bright White
                100 => self.bg_color = Color::rgb(127, 127, 127),
                101 => self.bg_color = Color::rgb(255, 0, 0),
                102 => self.bg_color = Color::rgb(0, 255, 0),
                103 => self.bg_color = Color::rgb(255, 255, 0),
                104 => self.bg_color = Color::rgb(92, 92, 255),
                105 => self.bg_color = Color::rgb(255, 0, 255),
                106 => self.bg_color = Color::rgb(0, 255, 255),
                107 => self.bg_color = Color::rgb(255, 255, 255),
                _ => {}
            }
        }

        if bold {
            self.fg_color.r = self.fg_color.r.saturating_add(40);
            self.fg_color.g = self.fg_color.g.saturating_add(40);
            self.fg_color.b = self.fg_color.b.saturating_add(40);
        }
    }
}

impl core::fmt::Write for GraphicsConsole {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &byte in s.as_bytes() {
            self.write_char(byte);
        }
        self.gpu.blit();
        Ok(())
    }
}

/// The globally active graphics console, if initialized.
pub static GRAPHICS_CONSOLE: Mutex<Option<GraphicsConsole>> = Mutex::new(None);

/// Global flag to disable mirroring standard kprint/serial outputs to the graphics console.
pub static DISABLE_CONSOLE_MIRROR: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(true);

/// Probes the PCI bus for Bochs graphics adapter, switches video modes,
/// renders boot splash, and registers the GPU driver.
pub fn init() {
    let devices = crate::drivers::bus::pci::find_device(0x1234, 0x1111);
    if devices.is_empty() {
        crate::kprintln!("[gpu] No Bochs VBE display adapter found on PCI bus.");
        return;
    }

    let dev = &devices[0];
    let cmd = crate::drivers::bus::pci::read_config(dev.bus, dev.device, dev.function, 0x04);
    crate::drivers::bus::pci::write_config(dev.bus, dev.device, dev.function, 0x04, cmd | 0x06); // memory space + bus master

    let bar0 = crate::drivers::bus::pci::read_config(dev.bus, dev.device, dev.function, 0x10);
    let lfb_phys = (bar0 & 0xFFFFFFF0) as u64;

    let width = 1024;
    let height = 768;
    let bpp = 32;
    let size = (width as u64) * (height as u64) * 4;

    // Map the physical framebuffer memory range (BAR 0) into the higher-half virtual address space.
    let lfb_virt = 0xffff_c000_0000_0000u64;
    let page_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE;
    let num_pages = (size + 4095) / 4096;
    for i in 0..num_pages {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(lfb_virt + i * 4096));
        let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(lfb_phys + i * 4096));
        // SAFETY: We map the hardware framebuffer BAR0 memory range (which is mapped to a physical address
        // by the PCI host controller) into a dedicated higher-half kernel virtual address space with
        // caching disabled (NO_CACHE) to ensure immediate write visibility.
        unsafe {
            crate::memory::r#virtual::map_page(page, frame, page_flags)
                .expect("Failed to map physical framebuffer page");
        }
    }

    // Configure video mode: 1024x768x32
    write_vbe(VBE_DISPI_INDEX_ENABLE, VBE_DISPI_DISABLED);
    write_vbe(VBE_DISPI_INDEX_XRES, width as u16);
    write_vbe(VBE_DISPI_INDEX_YRES, height as u16);
    write_vbe(VBE_DISPI_INDEX_BPP, bpp as u16);
    write_vbe(
        VBE_DISPI_INDEX_ENABLE,
        VBE_DISPI_ENABLED | VBE_DISPI_LFB_ENABLED,
    );

    let mut backbuffer = Vec::with_capacity((width * height) as usize);
    backbuffer.resize((width * height) as usize, 0);

    let gpu = alloc::sync::Arc::new(BochsGpu {
        lfb_phys,
        lfb_virt,
        width,
        height,
        bpp,
        size,
        backbuffer: Mutex::new(backbuffer),
    });

    let mut console = GraphicsConsole {
        gpu: gpu.clone(),
        cursor_x: 0,
        cursor_y: 0,
        fg_color: Color::WHITE,
        bg_color: Color::BLACK,
        ansi_state: AnsiState::Normal,
    };

    // Draw loading splash
    console.draw_splash();
    gpu.blit();

    // Register with driver manager
    let gpu_device = BochsGpuDevice {
        gpu: ArcMutexGpu(gpu),
    };
    crate::drivers::register_driver(gpu_device.info());

    *GRAPHICS_CONSOLE.lock() = Some(console);
    crate::kprintln!("[gpu] Bochs/VBE GPU driver initialized with graphics console.");
}
