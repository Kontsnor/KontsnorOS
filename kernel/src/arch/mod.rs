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

//! Architecture-specific code.
//!
//! This module provides abstractions over hardware-specific functionality.
//! Currently only x86_64 is supported, but the module structure allows
//! for easy addition of other architectures (e.g., aarch64).

#[cfg(target_arch = "x86_64")]
pub mod x86_64;
