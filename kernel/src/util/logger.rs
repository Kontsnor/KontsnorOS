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

//! Kernel logging framework.
//!
//! Provides structured logging levels for kernel messages.

/// Log levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    /// Detailed debug information.
    Debug,
    /// General informational messages.
    Info,
    /// Warning conditions.
    Warn,
    /// Error conditions.
    Error,
    /// Critical conditions — system may be unusable.
    Critical,
}

/// Current minimum log level.
static LOG_LEVEL: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

/// Set the minimum log level.
pub fn set_level(level: LogLevel) {
    LOG_LEVEL.store(level as u8, core::sync::atomic::Ordering::Relaxed);
}

/// Check if a log level is enabled.
pub fn is_enabled(level: LogLevel) -> bool {
    level as u8 >= LOG_LEVEL.load(core::sync::atomic::Ordering::Relaxed)
}
