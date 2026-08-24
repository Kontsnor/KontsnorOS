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

//! Driver memory allocation helpers.
//!
//! Provides allocation primitives specifically designed for
//! driver use, with proper alignment and page boundary handling.

/// A page-aligned memory allocation.
#[derive(Debug)]
pub struct PageAllocation {
    /// Virtual address of the allocation.
    pub virt_addr: u64,
    /// Physical address of the allocation.
    pub phys_addr: u64,
    /// Number of pages allocated.
    pub num_pages: usize,
}
