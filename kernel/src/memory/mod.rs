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

//! Memory management subsystem for KontsnorOS.
//!
//! This module provides:
//! - Physical memory frame allocation
//! - Virtual memory / page table management
//! - Kernel heap allocation
//! - Type-safe address wrappers

pub mod address;
pub mod heap;
pub mod page_cache;
pub mod physical;
pub mod r#virtual;

/// Page size on x86_64 (4 KiB).
pub const PAGE_SIZE: usize = 4096;
