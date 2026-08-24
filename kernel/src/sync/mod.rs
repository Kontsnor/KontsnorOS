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

//! Synchronization primitives for KontsnorOS.
//!
//! Provides kernel-level synchronization mechanisms:
//! - Spinlock — busy-waiting lock for short critical sections
//! - Mutex — sleeping lock for longer critical sections
//! - RwLock — reader-writer lock for shared data

pub mod mutex;
pub mod rwlock;
pub mod spinlock;
pub mod wait_queue;
